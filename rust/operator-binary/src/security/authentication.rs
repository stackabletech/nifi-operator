use indoc::{formatdoc, indoc};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{
        self,
        pod::{PodBuilder, container::ContainerBuilder},
    },
    crd::authentication::{ldap, oidc, r#static},
    k8s_openapi::api::core::v1::{KeyToPath, SecretVolumeSource, Volume},
};

use crate::{
    crd::{authentication::AuthenticationClassResolved, v1alpha1},
    security::oidc::build_oidc_admin_password_secret_name,
};

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

    #[snafu(display(
        "The authentication class provider [{authentication_class_provider}] is not supported by NiFi."
    ))]
    AuthenticationClassProviderNotSupported {
        authentication_class_provider: String,
    },

    #[snafu(display("Failed to add LDAP volumes and volumeMounts to the Pod and containers"))]
    AddLdapVolumes {
        source: stackable_operator::crd::authentication::ldap::v1alpha1::Error,
    },

    #[snafu(display("Failed to add OIDC volumes and volumeMounts to the Pod and containers"))]
    AddOidcVolumes {
        source: stackable_operator::commons::tls_verification::TlsClientDetailsError,
    },

    #[snafu(display(
        "The LDAP AuthenticationClass is missing the bind credentials. Currently the NiFi operator only supports connecting to LDAP servers using bind credentials"
    ))]
    LdapAuthenticationClassMissingBindCredentials {},

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },
}

#[allow(clippy::large_enum_variant)]
pub enum NifiAuthenticationConfig {
    SingleUser {
        provider: r#static::v1alpha1::AuthenticationProvider,
    },
    Ldap {
        provider: ldap::v1alpha1::AuthenticationProvider,
    },
    Oidc {
        provider: oidc::v1alpha1::AuthenticationProvider,
        oidc: oidc::v1alpha1::ClientAuthenticationOptions,
        nifi: v1alpha1::NifiCluster,
    },
}

impl NifiAuthenticationConfig {
    pub fn get_authentication_config(&self) -> Result<String, Error> {
        let mut login_identity_provider_xml = indoc! {r#"
            <?xml version="1.0" encoding="UTF-8" standalone="no"?>
            <loginIdentityProviders>
        "#}
        .to_string();

        match &self {
            Self::SingleUser { .. } | Self::Oidc { .. } => {
                login_identity_provider_xml.push_str(&formatdoc! {r#"
                    <provider>
                        <identifier>login-identity-provider</identifier>
                        <class>org.apache.nifi.authentication.single.user.SingleUserLoginIdentityProvider</class>
                        <property name="Username">{STACKABLE_ADMIN_USERNAME}</property>
                        <property name="Password">${{env:STACKABLE_ADMIN_PASSWORD}}</property>
                    </provider>
                "#,
                });
            }
            Self::Ldap { provider } => {
                login_identity_provider_xml.push_str(&get_ldap_login_identity_provider(provider)?);
            }
        }

        login_identity_provider_xml.push_str(indoc! {r#"
            </loginIdentityProviders>
        "#});

        Ok(login_identity_provider_xml)
    }

    pub fn get_user_and_password_file_paths(&self) -> (String, String) {
        let mut admin_username_file = String::new();
        let mut admin_password_file = String::new();
        match &self {
            Self::SingleUser { .. } | Self::Oidc { .. } => {
                admin_password_file =
                    format!("{STACKABLE_USER_VOLUME_MOUNT_PATH}/{STACKABLE_ADMIN_USERNAME}");
            }
            Self::Ldap { provider } => {
                if let Some((user_path, password_path)) = provider.bind_credentials_mount_paths() {
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
            Self::SingleUser { .. } => {
                let (_, admin_password_file) = self.get_user_and_password_file_paths();
                commands.extend(vec![
                    format!("export STACKABLE_ADMIN_PASSWORD=\"$(cat {admin_password_file} | java -jar /bin/stackable-bcrypt.jar)\""),
                ]);
            }
            Self::Ldap { provider } => {
                if let Some(ca_path) = provider.tls.tls_ca_cert_mount_path() {
                    commands.push(add_cert_to_truststore(&ca_path, STACKABLE_SERVER_TLS_DIR));
                }
            }
            Self::Oidc { provider, .. } => {
                let (_, admin_password_file) = self.get_user_and_password_file_paths();
                commands.push(format!("export STACKABLE_ADMIN_PASSWORD=\"$(cat {admin_password_file} | java -jar /bin/stackable-bcrypt.jar)\""));
                if let Some(ca_path) = provider.tls.tls_ca_cert_mount_path() {
                    commands.push(add_cert_to_truststore(&ca_path, STACKABLE_SERVER_TLS_DIR));
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
        mut container_builders: Vec<&mut ContainerBuilder>,
    ) -> Result<(), Error> {
        match &self {
            Self::SingleUser { provider } => {
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
                pod_builder
                    .add_volume(admin_volume)
                    .context(AddVolumeSnafu)?;

                for cb in container_builders {
                    cb.add_volume_mount(STACKABLE_ADMIN_USERNAME, STACKABLE_USER_VOLUME_MOUNT_PATH)
                        .context(AddVolumeMountSnafu)?;
                }
            }
            Self::Ldap { provider } => {
                provider
                    .add_volumes_and_mounts(pod_builder, container_builders)
                    .context(AddLdapVolumesSnafu)?;
            }
            Self::Oidc { provider, nifi, .. } => {
                let admin_volume = Volume {
                    name: STACKABLE_ADMIN_USERNAME.to_string(),
                    secret: Some(SecretVolumeSource {
                        secret_name: Some(build_oidc_admin_password_secret_name(nifi)),
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
                pod_builder
                    .add_volume(admin_volume)
                    .context(AddVolumeSnafu)?;

                for cb in &mut container_builders {
                    cb.add_volume_mount(STACKABLE_ADMIN_USERNAME, STACKABLE_USER_VOLUME_MOUNT_PATH)
                        .context(AddVolumeMountSnafu)?;
                }

                provider
                    .tls
                    .add_volumes_and_mounts(pod_builder, container_builders)
                    .context(AddOidcVolumesSnafu)?;
            }
        }

        Ok(())
    }

    pub fn try_from(
        auth_classes_resolved: Vec<AuthenticationClassResolved>,
    ) -> Result<Self, Error> {
        // Currently only one auth mechanism is supported in NiFi. This is checked in
        // rust/crd/src/authentication.rs and just a fail-safe here. For Future changes,
        // this is not just a "from" without error handling
        let auth_class_resolved = auth_classes_resolved
            .first()
            .context(SingleAuthenticationMechanismSupportedSnafu)?;

        match &auth_class_resolved {
            AuthenticationClassResolved::Static { provider } => Ok(Self::SingleUser {
                provider: provider.clone(),
            }),
            AuthenticationClassResolved::Ldap { provider } => Ok(Self::Ldap {
                provider: provider.clone(),
            }),
            AuthenticationClassResolved::Oidc {
                provider,
                oidc,
                nifi,
            } => Ok(Self::Oidc {
                provider: provider.clone(),
                oidc: oidc.clone(),
                nifi: nifi.clone(),
            }),
        }
    }
}

/// Adds a PEM file to configured PKCS12 truststore (using the [`STACKABLE_TLS_STORE_PASSWORD`]
/// password)
fn add_cert_to_truststore(cert_file: &str, destination_directory: &str) -> String {
    let truststore = format!("{destination_directory}/truststore.p12");
    format!(
        "cert-tools generate-pkcs12-truststore --pkcs12 {truststore}:{STACKABLE_TLS_STORE_PASSWORD} --pem {cert_file} --out {truststore} --out-password {STACKABLE_TLS_STORE_PASSWORD}"
    )
}

fn get_ldap_login_identity_provider(
    ldap: &ldap::v1alpha1::AuthenticationProvider,
) -> Result<String, Error> {
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
