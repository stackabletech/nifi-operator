use indoc::{formatdoc, indoc};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::builder::pod::{container::ContainerBuilder, PodBuilder};
use stackable_operator::commons::authentication::{ldap, static_};
use stackable_operator::commons::authentication::{
    AuthenticationClass, AuthenticationClassProvider,
};
use stackable_operator::k8s_openapi::api::core::v1::{KeyToPath, SecretVolumeSource, Volume};

pub const STACKABLE_ADMIN_USERNAME: &str = "admin";

const STACKABLE_USER_VOLUME_MOUNT_PATH: &str = "/stackable/users";

pub const LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME: &str = "login-identity-providers.xml";
pub const AUTHORIZERS_XML_FILE_NAME: &str = "authorizers.xml";

pub const STACKABLE_SERVER_TLS_DIR: &str = "/stackable/server_tls";
pub const STACKABLE_TLS_STORE_PASSWORD: &str = "secret";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("Only one authentication mechanism is supported by NiFi."))]
    SingleAuthenticationMechanismSupported,

    #[snafu(display("The authentication class provider [{authentication_class_provider}] is not supported by NiFi."))]
    AuthenticationClassProviderNotSupported {
        authentication_class_provider: String,
    },

    #[snafu(display("Failed to add LDAP volumes and volumeMounts to the Pod and containers"))]
    AddLdapVolumes {
        source: stackable_operator::commons::authentication::ldap::Error,
    },

    #[snafu(display(
        "The LDAP AuthenticationClass is missing the bind credentials. Currently the NiFi operator only supports connecting to LDAP servers using bind credentials"
    ))]
    LdapAuthenticationClassMissingBindCredentials {},
}

#[allow(clippy::large_enum_variant)]
pub enum NifiAuthenticationConfig {
    SingleUser(static_::AuthenticationProvider),
    Ldap(ldap::AuthenticationProvider),
}

impl NifiAuthenticationConfig {
    pub fn get_auth_config(&self) -> Result<(String, String), Error> {
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

        match &self {
            Self::SingleUser(_) => {
                login_identity_provider_xml.push_str(&formatdoc! {r#"
                    <provider>
                        <identifier>login-identity-provider</identifier>
                        <class>org.apache.nifi.authentication.single.user.SingleUserLoginIdentityProvider</class>
                        <property name="Username">{STACKABLE_ADMIN_USERNAME}</property>
                        <property name="Password">${{env:STACKABLE_ADMIN_PASSWORD}}</property>
                    </provider>
                "#,
                });

                authorizers_xml.push_str(indoc! {r#"
                    <authorizer>
                        <identifier>authorizer</identifier>
                        <class>org.apache.nifi.authorization.single.user.SingleUserAuthorizer</class>
                    </authorizer>
                "#});
            }
            Self::Ldap(ldap) => {
                login_identity_provider_xml.push_str(&get_ldap_login_identity_provider(ldap)?);
                authorizers_xml.push_str(&get_ldap_authorizer(ldap)?);
            }
        }

        login_identity_provider_xml.push_str(indoc! {r#"
            </loginIdentityProviders>
        "#});
        authorizers_xml.push_str(indoc! {r#"
            </authorizers>
        "#});

        Ok((login_identity_provider_xml, authorizers_xml))
    }

    pub fn get_user_and_password_file_paths(&self) -> (String, String) {
        let mut admin_username_file = String::new();
        let mut admin_password_file = String::new();
        match &self {
            Self::SingleUser(_) => {
                admin_password_file =
                    format!("{STACKABLE_USER_VOLUME_MOUNT_PATH}/{STACKABLE_ADMIN_USERNAME}");
            }
            Self::Ldap(ldap) => {
                if let Some((user_path, password_path)) = ldap.bind_credentials_mount_paths() {
                    admin_username_file = user_path;
                    admin_password_file = password_path;
                }
            }
        }
        (admin_username_file, admin_password_file)
    }

    pub fn get_additional_container_args(&self) -> Vec<String> {
        let mut commands = Vec::new();
        match &self {
            Self::SingleUser(_) => {
                let (_, admin_password_file) = self.get_user_and_password_file_paths();
                commands.extend(vec![
                    format!("export STACKABLE_ADMIN_PASSWORD=\"$(cat {admin_password_file} | java -jar /bin/stackable-bcrypt.jar)\""),
                ]);
            }
            Self::Ldap(ldap) => {
                if let Some(ca_path) = ldap.tls.tls_ca_cert_mount_path() {
                    commands.extend(vec![
                        "echo Adding LDAP tls cert to global truststore".to_string(),
                        format!("keytool -importcert -file {ca_path} -keystore {STACKABLE_SERVER_TLS_DIR}/truststore.p12 -storetype pkcs12 -noprompt -alias ldap_ca_cert -storepass {STACKABLE_TLS_STORE_PASSWORD}"),
                    ]);
                }
            }
        }
        commands
    }

    /// Returns
    /// - A list of extra commands for the init container
    pub fn add_volumes_and_mounts(
        &self,
        pod_builder: &mut PodBuilder,
        container_builders: Vec<&mut ContainerBuilder>,
    ) -> Result<(), Error> {
        match &self {
            Self::SingleUser(provider) => {
                let admin_volume = Volume {
                    name: STACKABLE_ADMIN_USERNAME.to_string(),
                    secret: Some(SecretVolumeSource {
                        secret_name: Some(provider.user_credentials_secret.name.to_string()),
                        optional: Some(false),
                        items: Some(vec![KeyToPath {
                            key: STACKABLE_ADMIN_USERNAME.to_string(),
                            path: STACKABLE_ADMIN_USERNAME.to_string(),
                            ..KeyToPath::default()
                        }]),
                        ..SecretVolumeSource::default()
                    }),
                    ..Volume::default()
                };
                pod_builder.add_volume(admin_volume);

                for cb in container_builders {
                    cb.add_volume_mount(STACKABLE_ADMIN_USERNAME, STACKABLE_USER_VOLUME_MOUNT_PATH);
                }
            }
            Self::Ldap(ldap) => {
                ldap.add_volumes_and_mounts(pod_builder, container_builders)
                    .context(AddLdapVolumesSnafu)?;
            }
        }

        Ok(())
    }

    pub fn try_from(auth_classes: Vec<AuthenticationClass>) -> Result<Self, Error> {
        // Currently only one auth mechanism is supported in NiFi. This is checked in
        // rust/crd/src/authentication.rs and just a fail-safe here. For Future changes,
        // this is not just a "from" without error handling
        let auth_class = auth_classes
            .first()
            .context(SingleAuthenticationMechanismSupportedSnafu)?;

        match &auth_class.spec.provider {
            AuthenticationClassProvider::Static(static_provider) => {
                Ok(Self::SingleUser(static_provider.clone()))
            }
            AuthenticationClassProvider::Ldap(ldap_provider) => {
                Ok(Self::Ldap(ldap_provider.clone()))
            }
            AuthenticationClassProvider::Tls(_) | AuthenticationClassProvider::Oidc(_) => {
                Err(Error::AuthenticationClassProviderNotSupported {
                    authentication_class_provider: auth_class.spec.provider.to_string(),
                })
            }
        }
    }
}

fn get_ldap_login_identity_provider(ldap: &ldap::AuthenticationProvider) -> Result<String, Error> {
    let mut search_filter = ldap.search_filter.clone();

    // If no search_filter is specified we will set a default filter that just searches for the user logging in using the specified uid field name
    if search_filter.is_empty() {
        search_filter
            .push_str(format!("{uidField}={{0}}", uidField = ldap.ldap_field_names.uid).as_str());
    }

    let (username_file, password_file) = ldap
        .bind_credentials_mount_paths()
        .context(LdapAuthenticationClassMissingBindCredentialsSnafu)?;

    Ok(formatdoc! {r#"
        <provider>
            <identifier>login-identity-provider</identifier>
            <class>org.apache.nifi.ldap.LdapProvider</class>
            <property name="Authentication Strategy">{authentication_strategy}</property>

            <property name="Manager DN">${{file:UTF-8:{username_file}}}</property>
            <property name="Manager Password">${{file:UTF-8:{password_file}}}</property>

            <property name="Referral Strategy">THROW</property>
            <property name="Connect Timeout">10 secs</property>
            <property name="Read Timeout">10 secs</property>

            <property name="Url">{protocol}://{hostname}:{port}</property>
            <property name="User Search Base">{search_base}</property>
            <property name="User Search Filter">{search_filter}</property>

            <property name="TLS - Client Auth">NONE</property>
            <property name="TLS - Keystore">{keystore_path}/keystore.p12</property>
            <property name="TLS - Keystore Password">{STACKABLE_TLS_STORE_PASSWORD}</property>
            <property name="TLS - Keystore Type">PKCS12</property>
            <property name="TLS - Truststore">{keystore_path}/truststore.p12</property>
            <property name="TLS - Truststore Password">{STACKABLE_TLS_STORE_PASSWORD}</property>
            <property name="TLS - Truststore Type">PKCS12</property>
            <property name="TLS - Protocol">TLSv1.2</property>
            <property name="TLS - Shutdown Gracefully">true</property>

            <property name="Identity Strategy">USE_DN</property>
            <property name="Authentication Expiration">7 days</property>
        </provider>
    "#,
        authentication_strategy = if ldap.bind_credentials_mount_paths().is_some() {
            if ldap.tls.uses_tls() {
                "LDAPS"
            } else {
                "SIMPLE"
            }
        } else {
            "ANONYMOUS"
        },
        protocol = if ldap.tls.uses_tls() {
            "ldaps"
        } else {
            "ldap"
        },
        hostname = ldap.hostname,
        port = ldap.port(),
        search_base = ldap.search_base,
        keystore_path = STACKABLE_SERVER_TLS_DIR,
    })
}

fn get_ldap_authorizer(ldap: &ldap::AuthenticationProvider) -> Result<String, Error> {
    let (username_file, _) = ldap
        .bind_credentials_mount_paths()
        .context(LdapAuthenticationClassMissingBindCredentialsSnafu)?;

    Ok(formatdoc! {r#"
        <userGroupProvider>
            <identifier>file-user-group-provider</identifier>
            <class>org.apache.nifi.authorization.FileUserGroupProvider</class>
            <property name="Users File">./conf/users.xml</property>

            <!-- As we currently don't have authorization (including admin user) configurable we simply paste in the ldap bind user in here -->
            <!-- In the future the whole authorization may be reworked to OPA -->
            <property name="Initial User Identity admin">${{file:UTF-8:{username_file}}}</property>

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
            <property name="Initial Admin Identity">${{file:UTF-8:{username_file}}}</property>

            <!-- As the secret-operator provides the NiFi nodes with cert with a common name of "generated certificate for pod" we have to put that here -->
            <property name="Node Identity other-nifis">CN=generated certificate for pod</property>
        </accessPolicyProvider>

        <authorizer>
            <identifier>authorizer</identifier>
            <class>org.apache.nifi.authorization.StandardManagedAuthorizer</class>
            <property name="Access Policy Provider">file-access-policy-provider</property>
        </authorizer>
    "#})
}
