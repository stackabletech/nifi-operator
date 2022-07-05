use indoc::{formatdoc, indoc};
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_operator::builder::{
    ObjectMetaBuilder, SecretOperatorVolumeSourceBuilder, VolumeBuilder,
};
use stackable_operator::client::Client;
use stackable_operator::commons::authentication::{
    AuthenticationClass, AuthenticationClassProvider,
};
use stackable_operator::commons::ldap::LdapAuthenticationProvider;
use stackable_operator::commons::tls::{CaCert, Tls, TlsServerVerification, TlsVerification};
use stackable_operator::k8s_openapi::api::core::v1::{Secret, SecretVolumeSource, Volume};
use stackable_operator::kube::runtime::reflector::ObjectRef;
use stackable_operator::schemars::{self, JsonSchema};
use std::collections::BTreeMap;

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("Failed to find referenced secret {obj_ref}"))]
    MissingSecret {
        source: stackable_operator::error::Error,
        obj_ref: ObjectRef<Secret>,
    },
    #[snafu(display("Error when communication with apiserver while:  {reason} "))]
    Kube {
        source: stackable_operator::error::Error,
        reason: String,
    },
    MissingSecretReference {
        secret: String,
    },
    #[snafu(display(
        "A required value was not found when parsing the authentication config: [{}]",
        value
    ))]
    MissingRequiredValue {
        value: String,
    },
    #[snafu(display(
        "Unable to load admin credentials and auto-generation is disabled: [{}]",
        message
    ))]
    AdminCredentials {
        message: String,
    },
    #[snafu(display("failed to retrieve AuthenticationClass {authentication_class}"))]
    AuthenticationClassRetrieval {
        source: stackable_operator::error::Error,
        authentication_class: ObjectRef<AuthenticationClass>,
    },
    #[snafu(display("Nifi doesn't support the AuthenticationClass provider {authentication_class_provider} from AuthenticationClass {authentication_class}"))]
    AuthenticationClassProviderNotSupported {
        authentication_class_provider: String,
        authentication_class: ObjectRef<AuthenticationClass>,
    },
    #[snafu(display("Nifi doesn't support skipping the LDAP TLS verification of the AuthenticationClass {authentication_class}"))]
    NoLdapTlsVerificationNotSupported {
        authentication_class: ObjectRef<AuthenticationClass>,
    },
}

pub const SINGLEUSER_DEFAULT_ADMIN: &str = "admin";
pub const SINGLEUSER_USER_KEY: &str = "username";
pub const SINGLEUSER_PASSWORD_KEY: &str = "password";

pub const AUTH_VOLUME_NAME: &str = "adminuser";
pub const AUTH_VOLUME_MOUNT_PATH: &str = "/stackable/adminuser";

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiAuthenticationConfig {
    pub method: NifiAuthenticationMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_anonymous_access: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, strum::Display)]
#[serde(rename_all = "camelCase")]
pub enum NifiAuthenticationMethod {
    #[serde(rename_all = "camelCase")]
    SingleUser {
        admin_credentials_secret: String,
        #[serde(default)]
        auto_generate: bool,
    },
    AuthenticationClass(String),
}

impl NifiAuthenticationConfig {
    pub fn allow_anonymous(&self) -> bool {
        self.allow_anonymous_access.unwrap_or(false)
    }
}

/// Returns login_identity_provider.xml and authorizers.xml
pub async fn get_auth_configs(
    client: &Client,
    config: &NifiAuthenticationConfig,
    current_namespace: &str,
) -> Result<(String, String), Error> {
    let mut login_identity_provider_xml = indoc! {r#"
        <?xml version="1.0" encoding="UTF-8" standalone="no"?>
        <loginIdentityProviders>
    "#}
    .to_string();
    let mut authorizers_xml = indoc! {r#"
        <?xml version="1.0" encoding="UTF-8" standalone="yes"?>
        <authorizers>
    "#}
    .to_string();

    match &config.method {
        NifiAuthenticationMethod::SingleUser {
            admin_credentials_secret,
            auto_generate,
        } => {
            // Check if the referenced secret exists and contains all necessary keys, otherwise
            // generate random password and default user
            check_or_generate_admin_credentials(
                client,
                admin_credentials_secret,
                current_namespace,
                auto_generate,
            )
            .await?;

            login_identity_provider_xml.push_str(indoc! {r#"
                <provider>
                    <identifier>login-identity-provider</identifier>
                    <class>org.apache.nifi.authentication.single.user.SingleUserLoginIdentityProvider</class>
                    <property name="Username">xxx_singleuser_username_xxx</property>
                    <property name="Password">xxx_singleuser_password_xxx</property>
                </provider>
            "#});

            authorizers_xml.push_str(indoc! {r#"
                <authorizer>
                    <identifier>authorizer</identifier>
                    <class>org.apache.nifi.authorization.single.user.SingleUserAuthorizer</class>
                </authorizer>
            "#});
        }
        NifiAuthenticationMethod::AuthenticationClass(authentication_class_name) => {
            let authentication_class =
                AuthenticationClass::resolve(client, authentication_class_name)
                    .await
                    .context(AuthenticationClassRetrievalSnafu {
                        authentication_class: ObjectRef::<AuthenticationClass>::new(
                            authentication_class_name,
                        ),
                    })?;

            match &authentication_class.spec.provider {
                AuthenticationClassProvider::Ldap(ldap) => {
                    if let Some(Tls {
                        verification: TlsVerification::None {},
                    }) = &ldap.tls
                    {
                        return NoLdapTlsVerificationNotSupportedSnafu {
                            authentication_class: ObjectRef::<AuthenticationClass>::new(
                                authentication_class_name,
                            ),
                        }
                        .fail();
                    }
                    login_identity_provider_xml.push_str(&get_ldap_login_identity_provider(ldap));
                    authorizers_xml.push_str(&get_ldap_authorizer(ldap));
                }
                _ => {
                    return AuthenticationClassProviderNotSupportedSnafu {
                        authentication_class_provider: authentication_class
                            .spec
                            .provider
                            .to_string(),
                        authentication_class: ObjectRef::<AuthenticationClass>::new(
                            authentication_class_name,
                        ),
                    }
                    .fail()
                }
            }
        }
    }

    login_identity_provider_xml.push_str(indoc! {r#"
        </loginIdentityProviders>
    "#});
    authorizers_xml.push_str(indoc! {r#"
        </authorizers>
    "#});

    Ok((authorizers_xml, login_identity_provider_xml))
}

/// Returns a BTreeMap of volumes to add and a list of extra commands for the init container
pub async fn get_auth_volumes(
    client: &Client,
    method: &NifiAuthenticationMethod,
) -> Result<(BTreeMap<String, (String, Volume)>, Vec<String>), Error> {
    let mut volumes = BTreeMap::new();
    let mut commands = Vec::new();

    match method {
        NifiAuthenticationMethod::SingleUser {
            admin_credentials_secret,
            ..
        } => {
            let admin_volume = Volume {
                name: AUTH_VOLUME_NAME.to_string(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(admin_credentials_secret.to_string()),
                    ..SecretVolumeSource::default()
                }),
                ..Volume::default()
            };
            volumes.insert(
                AUTH_VOLUME_NAME.to_string(),
                (AUTH_VOLUME_MOUNT_PATH.to_string(), admin_volume),
            );

            commands.extend(vec![
                "echo Replacing admin username and password in login-identity-provider.xml (if configured)".to_string(),
                "sed -i \"s|xxx_singleuser_username_xxx|$(cat /stackable/adminuser/username)|g\" /stackable/nifi/conf/login-identity-providers.xml".to_string(),
                "sed -i \"s|xxx_singleuser_password_xxx|$(cat /stackable/adminuser/password | java -jar /bin/stackable-bcrypt.jar)|g\" /stackable/nifi/conf/login-identity-providers.xml".to_string(),
                ]
            );
        }
        NifiAuthenticationMethod::AuthenticationClass(authentication_class_name) => {
            let authentication_class =
                AuthenticationClass::resolve(client, authentication_class_name)
                    .await
                    .context(AuthenticationClassRetrievalSnafu {
                        authentication_class: ObjectRef::<AuthenticationClass>::new(
                            authentication_class_name,
                        ),
                    })?;

            if let AuthenticationClassProvider::Ldap(ldap) = authentication_class.spec.provider {
                if let Some(credentials) = ldap.bind_credentials {
                    let volume_name = format!("{authentication_class_name}-bind-credentials");
                    let secret_volume = VolumeBuilder::new(&volume_name)
                        .ephemeral(
                            SecretOperatorVolumeSourceBuilder::new(credentials.secret_class)
                                .build(),
                        )
                        .build();

                    volumes.insert(
                        volume_name.clone(),
                        (format!("/stackable/secrets/{volume_name}"), secret_volume),
                    );

                    commands.extend(vec![
                        "echo Replacing ldap bind username and password in login-identity-provider.xml".to_string(),
                        format!("sed -i \"s|xxx_ldap_bind_username_xxx|$(cat /stackable/secrets/{volume_name}/user)|g\" /stackable/nifi/conf/login-identity-providers.xml"),
                        format!("sed -i \"s|xxx_ldap_bind_password_xxx|$(cat /stackable/secrets/{volume_name}/password)|g\" /stackable/nifi/conf/login-identity-providers.xml"),
                        format!("sed -i \"s|xxx_ldap_bind_username_xxx|$(cat /stackable/secrets/{volume_name}/user)|g\" /stackable/nifi/conf/authorizers.xml"),
                        ]
                    );
                }
                if let Some(Tls {
                    verification:
                        TlsVerification::Server(TlsServerVerification {
                            ca_cert: CaCert::SecretClass(secret_class_name),
                        }),
                }) = ldap.tls
                {
                    let volume_name = format!("{authentication_class_name}-tls-certificate");
                    let secret_volume = VolumeBuilder::new(&volume_name)
                        .ephemeral(
                            SecretOperatorVolumeSourceBuilder::new(secret_class_name).build(),
                        )
                        .build();

                    volumes.insert(
                        volume_name.clone(),
                        (
                            format!("/stackable/certificates/{volume_name}"),
                            secret_volume,
                        ),
                    );

                    commands.extend(vec![
                        "echo Adding LDAP tls cert to global truststore".to_string(),
                        format!("keytool -importcert -file /stackable/certificates/{volume_name}/ca.crt -keystore /stackable/keystore/truststore.p12 -storetype pkcs12 -noprompt -alias ldap_ca_cert -storepass secret"),
                        ]
                    );
                }
            }
        }
    }

    Ok((volumes, commands))
}

async fn check_or_generate_admin_credentials(
    client: &Client,
    secret_name: &str,
    secret_namespace: &str,
    auto_generate: &bool,
) -> Result<bool, Error> {
    match client
        .exists::<Secret>(secret_name, Some(secret_namespace))
        .await
        .with_context(|_| KubeSnafu {
            reason: format!(
                "checking if admin credential secret exists [{}/{}]",
                secret_name, secret_namespace
            ),
        })? {
        true => {
            // The secret exists, retrieve the content and check that all required keys are present
            // any missing keys will be filled with default or generated values
            let secret_content: Secret = client
                .get::<Secret>(secret_name, Some(secret_namespace))
                .await
                .with_context(|_| MissingSecretSnafu {
                    obj_ref: ObjectRef::new(secret_name).within(secret_namespace),
                })?;

            let mut additional_data = None;
            let empty_map = BTreeMap::new();

            // Check if user key is present, otherwise add to additional data
            if !secret_content
                .data
                .as_ref()
                .unwrap_or(&empty_map)
                .contains_key(SINGLEUSER_USER_KEY)
            {
                tracing::info!(
                    "key [{}] not found in secret [{}/{}], inserting default value of \"admin\"",
                    SINGLEUSER_USER_KEY,
                    secret_name,
                    secret_namespace
                );
                additional_data.get_or_insert(BTreeMap::new()).insert(
                    SINGLEUSER_USER_KEY.to_string(),
                    SINGLEUSER_DEFAULT_ADMIN.to_string(),
                );
            }

            // Check if password key is present, otherwise add to additional data
            if !secret_content
                .data
                .as_ref()
                .unwrap_or(&empty_map)
                .contains_key(SINGLEUSER_PASSWORD_KEY)
            {
                tracing::info!(
                    "key [{}] not found in secret [{}/{}], inserting generated password",
                    SINGLEUSER_PASSWORD_KEY,
                    secret_name,
                    secret_namespace
                );
                let generated_password = rand::thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(15)
                    .map(char::from)
                    .collect();
                additional_data
                    .get_or_insert(BTreeMap::new())
                    .insert(SINGLEUSER_PASSWORD_KEY.to_string(), generated_password);
            }

            // Apply patch to secret if any additional data was needed and return
            if additional_data.is_some() {
                // Check if we are allowed to auto generate and abort if not
                if !auto_generate {
                    return Err(Error::AdminCredentials {
                        message: format!(
                            "Admin credential secret [{}/{}] is missing keys: [{:?}]",
                            secret_name,
                            secret_namespace,
                            additional_data.unwrap().keys()
                        ),
                    });
                }
                tracing::debug!(
                    "patching keys [{:?}] in secret [{}/{}]",
                    additional_data.clone().unwrap_or_default().keys(),
                    secret_name,
                    secret_namespace,
                );
                let secret_patch = Secret {
                    metadata: ObjectMetaBuilder::new()
                        .namespace(secret_namespace)
                        .name(secret_name)
                        .build(),
                    string_data: additional_data,
                    ..Secret::default()
                };
                client.apply_patch("nificluster", &secret_patch, &secret_patch).await.with_context(|_| KubeSnafu {reason: format!{"patch admin credentialsecret [{}/{}]with missing data", secret_name, secret_namespace}})?;
                Ok(true)
            } else {
                // All needed keys are present, no need to change anything
                tracing::debug!(
                    "all required data for admin credentials found in secret [{}/{}]",
                    secret_name,
                    secret_namespace
                );
                Ok(false)
            }
        }
        false => {
            if !auto_generate {
                return Err(Error::AdminCredentials {
                    message: format!(
                        "Admin credential secret [{}/{}] does not exist.",
                        secret_name, secret_namespace
                    ),
                });
            }
            tracing::info!("No existing admin credentials found, generating a random password.");
            let password: String = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(15)
                .map(char::from)
                .collect();

            let mut secret_data = BTreeMap::new();

            secret_data.insert(
                SINGLEUSER_USER_KEY.to_string(),
                SINGLEUSER_DEFAULT_ADMIN.to_string(),
            );
            secret_data.insert(SINGLEUSER_PASSWORD_KEY.to_string(), password.to_string());

            let new_secret = Secret {
                metadata: ObjectMetaBuilder::new()
                    .namespace(secret_namespace)
                    .name(secret_name)
                    .build(),
                string_data: Some(secret_data),
                ..Secret::default()
            };
            client
                .create(&new_secret)
                .await
                .with_context(|_| KubeSnafu {
                    reason: format!(
                        "creating new secret for admincredentials: [{}/{}]",
                        secret_name, secret_namespace
                    ),
                })?;
            Ok(true)
        }
    }
}

fn get_ldap_login_identity_provider(ldap: &LdapAuthenticationProvider) -> String {
    let mut search_filter = ldap.search_filter.clone();

    // If no search_filter is specified we will set a default filter that just searches for the user logging in using the specified uid field name
    if search_filter.is_empty() {
        search_filter
            .push_str(format!("{uidField}={{0}}", uidField = ldap.ldap_field_names.uid).as_str());
    }

    formatdoc! {r#"
        <provider>
            <identifier>login-identity-provider</identifier>
            <class>org.apache.nifi.ldap.LdapProvider</class>
            <property name="Authentication Strategy">{authentication_strategy}</property>

            <property name="Manager DN">xxx_ldap_bind_username_xxx</property>
            <property name="Manager Password">xxx_ldap_bind_password_xxx</property>

            <property name="Referral Strategy">THROW</property>
            <property name="Connect Timeout">10 secs</property>
            <property name="Read Timeout">10 secs</property>

            <property name="Url">{protocol}://{hostname}:{port}</property>
            <property name="User Search Base">{search_base}</property>
            <property name="User Search Filter">{search_filter}</property>

            <property name="TLS - Client Auth">NONE</property>
            <property name="TLS - Keystore">/stackable/keystore/keystore.p12</property>
            <property name="TLS - Keystore Password">secret</property>
            <property name="TLS - Keystore Type">PKCS12</property>
            <property name="TLS - Truststore">/stackable/keystore/truststore.p12</property>
            <property name="TLS - Truststore Password">secret</property>
            <property name="TLS - Truststore Type">PKCS12</property>
            <property name="TLS - Protocol">TLSv1.2</property>
            <property name="TLS - Shutdown Gracefully">true</property>

            <property name="Identity Strategy">USE_DN</property>
            <property name="Authentication Expiration">7 days</property>
        </provider>
    "#,
        authentication_strategy = if ldap.bind_credentials.is_some() {
            if ldap.tls.is_some() {
                "LDAPS"
            } else {
                "SIMPLE"
            }
        } else {
            "ANONYMOUS"
        },
        protocol = if ldap.tls.is_some() {
            "ldaps"
        } else {
            "ldap"
        },
        hostname = ldap.hostname,
        port = ldap.port.unwrap_or_else(|| ldap.default_port()),
        search_base = ldap.search_base,
    }
}

fn get_ldap_authorizer(_ldap: &LdapAuthenticationProvider) -> String {
    formatdoc! {r#"
        <userGroupProvider>
            <identifier>file-user-group-provider</identifier>
            <class>org.apache.nifi.authorization.FileUserGroupProvider</class>
            <property name="Users File">./conf/users.xml</property>

            <!-- As we currently don't have authorization (including admin user) configurable we simply paste in the ldap bind user in here -->
            <!-- In the future the whole authorization may be reworked to OPA -->
            <property name="Initial User Identity admin">xxx_ldap_bind_username_xxx</property>

            <!-- As the secret-operator provides the NiFi nodes with cert with a common name of "generated certificate for pod" we have to put that here -->
            <property name="Initial User Identity other-nifis">CN=generated certificate for pod</property>
        </userGroupProvider>

        <accessPolicyProvider>
            <identifier>file-access-policy-provider</identifier>
            <class>org.apache.nifi.authorization.FileAccessPolicyProvider</class>
            <property name="User Group Provider">file-user-group-provider</property>
            <property name="Authorizations File">./conf/authorizations.xml</property>

            <!-- As we currently don't have authorization (including admin user) configurable we simply paste in the ldap bind user in here -->
            <!-- In the future the whole authorization may be reworked to OPA -->
            <property name="Initial Admin Identity">xxx_ldap_bind_username_xxx</property>

            <!-- As the secret-operator provides the NiFi nodes with cert with a common name of "generated certificate for pod" we have to put that here -->
            <property name="Node Identity other-nifis">CN=generated certificate for pod</property>
        </accessPolicyProvider>

        <authorizer>
            <identifier>authorizer</identifier>
            <class>org.apache.nifi.authorization.StandardManagedAuthorizer</class>
            <property name="Access Policy Provider">file-access-policy-provider</property>
        </authorizer>
    "#}
}
