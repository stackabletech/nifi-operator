use indoc::{formatdoc, indoc};
use snafu::{OptionExt, Snafu};
use stackable_operator::builder::{ContainerBuilder, PodBuilder};
use stackable_operator::commons::authentication::{
    AuthenticationClass, AuthenticationClassProvider, LdapAuthenticationProvider,
    StaticAuthenticationProvider,
};
use stackable_operator::k8s_openapi::api::core::v1::{KeyToPath, SecretVolumeSource, Volume};

pub const STACKABLE_ADMIN_USER_NAME: &str = "admin";

const STACKABLE_USER_VOLUME_MOUNT_PATH: &str = "/stackable/users";

const STACKABLE_SINGLE_USER_PASSWORD_PLACEHOLDER: &str = "xxx_singleuser_password_xxx";
const STACKABLE_LDAP_BIND_USER_NAME_PLACEHOLDER: &str = "xxx_ldap_bind_username_xxx";
const STACKABLE_LDAP_BIND_USER_PASSWORD_PLACEHOLDER: &str = "xxx_ldap_bind_password_xxx";

pub const LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME: &str = "login-identity-providers.xml";
pub const AUTHORIZERS_XML_FILE_NAME: &str = "authorizers.xml";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("Only one authentication mechanism is supported by NiFi."))]
    SingleAuthenticationMechanismSupported,
    #[snafu(display("The authentication class provider [{authentication_class_provider}] is not supported by NiFi."))]
    AuthenticationClassProviderNotSupported {
        authentication_class_provider: String,
    },
}

#[allow(clippy::large_enum_variant)]
pub enum NifiAuthenticationConfig {
    SingleUser(StaticAuthenticationProvider),
    Ldap(LdapAuthenticationProvider),
}

impl NifiAuthenticationConfig {
    pub fn get_auth_config(&self) -> (String, String) {
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
                        <property name="Username">{STACKABLE_ADMIN_USER_NAME}</property>
                        <property name="Password">{STACKABLE_SINGLE_USER_PASSWORD_PLACEHOLDER}</property>
                    </provider>
                "#});

                authorizers_xml.push_str(indoc! {r#"
                    <authorizer>
                        <identifier>authorizer</identifier>
                        <class>org.apache.nifi.authorization.single.user.SingleUserAuthorizer</class>
                    </authorizer>
                "#});
            }
            Self::Ldap(ldap) => {
                login_identity_provider_xml.push_str(&get_ldap_login_identity_provider(ldap));
                authorizers_xml.push_str(&get_ldap_authorizer(ldap));
            }
        }

        login_identity_provider_xml.push_str(indoc! {r#"
            </loginIdentityProviders>
        "#});
        authorizers_xml.push_str(indoc! {r#"
            </authorizers>
        "#});

        (login_identity_provider_xml, authorizers_xml)
    }

    pub fn get_user_and_password_file_paths(&self) -> (String, String) {
        let mut admin_username_file = String::new();
        let mut admin_password_file = String::new();
        match &self {
            Self::SingleUser(_) => {
                admin_password_file =
                    format!("{STACKABLE_USER_VOLUME_MOUNT_PATH}/{STACKABLE_ADMIN_USER_NAME}");
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
                    format!("echo 'Replacing {STACKABLE_ADMIN_USER_NAME} password in {LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME} (if configured)'"),
                    format!("sed -i \"s|{STACKABLE_SINGLE_USER_PASSWORD_PLACEHOLDER}|$(cat {admin_password_file} | java -jar /bin/stackable-bcrypt.jar)|g\" /stackable/nifi/conf/{LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME}"),
                ]
                );
            }
            Self::Ldap(ldap) => {
                if let Some((username_path, password_path)) = ldap.bind_credentials_mount_paths() {
                    commands.extend(vec![
                        format!("echo Replacing ldap bind username and password in {LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME}"),
                        format!("sed -i \"s|{STACKABLE_LDAP_BIND_USER_NAME_PLACEHOLDER}|$(cat {username_path})|g\" /stackable/nifi/conf/{LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME}"),
                        format!("sed -i \"s|{STACKABLE_LDAP_BIND_USER_PASSWORD_PLACEHOLDER}|$(cat {password_path})|g\" /stackable/nifi/conf/{LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME}"),
                        format!("sed -i \"s|{STACKABLE_LDAP_BIND_USER_NAME_PLACEHOLDER}|$(cat {username_path})|g\" /stackable/nifi/conf/{AUTHORIZERS_XML_FILE_NAME}"),
                    ]
                    );
                }
                if let Some(ca_path) = ldap.tls_ca_cert_mount_path() {
                    commands.extend(vec![
                        "echo Adding LDAP tls cert to global truststore".to_string(),
                        format!("keytool -importcert -file {ca_path} -keystore /stackable/keystore/truststore.p12 -storetype pkcs12 -noprompt -alias ldap_ca_cert -storepass secret"),
                    ]
                    );
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
    ) {
        match &self {
            Self::SingleUser(provider) => {
                let admin_volume = Volume {
                    name: STACKABLE_ADMIN_USER_NAME.to_string(),
                    secret: Some(SecretVolumeSource {
                        secret_name: Some(provider.user_credentials_secret.name.to_string()),
                        optional: Some(false),
                        items: Some(vec![KeyToPath {
                            key: STACKABLE_ADMIN_USER_NAME.to_string(),
                            path: STACKABLE_ADMIN_USER_NAME.to_string(),
                            ..KeyToPath::default()
                        }]),
                        ..SecretVolumeSource::default()
                    }),
                    ..Volume::default()
                };
                pod_builder.add_volume(admin_volume);

                for cb in container_builders {
                    cb.add_volume_mount(
                        STACKABLE_ADMIN_USER_NAME,
                        STACKABLE_USER_VOLUME_MOUNT_PATH,
                    );
                }
            }
            Self::Ldap(ldap) => {
                ldap.add_volumes_and_mounts(pod_builder, container_builders);
            }
        }
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
            AuthenticationClassProvider::Tls(_) => {
                Err(Error::AuthenticationClassProviderNotSupported {
                    authentication_class_provider: auth_class.spec.provider.to_string(),
                })
            }
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

            <property name="Manager DN">{STACKABLE_LDAP_BIND_USER_NAME_PLACEHOLDER}</property>
            <property name="Manager Password">{STACKABLE_LDAP_BIND_USER_PASSWORD_PLACEHOLDER}</property>

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
            <property name="Initial User Identity admin">{STACKABLE_LDAP_BIND_USER_NAME_PLACEHOLDER}</property>

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
            <property name="Initial Admin Identity">{STACKABLE_LDAP_BIND_USER_NAME_PLACEHOLDER}</property>

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
