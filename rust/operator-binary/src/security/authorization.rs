use indoc::{formatdoc, indoc};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    client::Client,
    crd::authentication::ldap,
    k8s_openapi::api::core::v1::{ConfigMap, ConfigMapKeySelector, EnvVar, EnvVarSource},
};

use super::authentication::NifiAuthenticationConfig;
use crate::crd::NifiAuthorization;

pub const OPA_TLS_VOLUME_NAME: &str = "opa-tls";
pub const OPA_TLS_MOUNT_PATH: &str = "/stackable/opa_tls";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display(
        "The LDAP AuthenticationClass is missing the bind credentials. Currently the NiFi operator only supports connecting to LDAP servers using bind credentials"
    ))]
    LdapAuthenticationClassMissingBindCredentials {},

    #[snafu(display("Failed to fetch OPA ConfigMap {configmap_name}"))]
    FetchOpaConfigMap {
        source: stackable_operator::client::Error,
        configmap_name: String,
        namespace: String,
    },
}

pub enum NifiAuthorizationConfig {
    Opa {
        configmap_name: String,
        cache_entry_time_to_live_secs: u64,
        cache_max_entries: u32,
        secret_class: Option<String>,
    },
    Default,
}

impl NifiAuthorizationConfig {
    pub async fn from(
        nifi_authorization: &Option<NifiAuthorization>,
        client: &Client,
        namespace: &str,
    ) -> Result<Self, Error> {
        let config = match nifi_authorization {
            Some(authorization_config) => match authorization_config.opa.clone() {
                Some(opa_config) => {
                    let configmap_name = opa_config.opa.config_map_name.clone();

                    // Resolve the secret class from the ConfigMap
                    let secret_class = client
                        .get::<ConfigMap>(&configmap_name, namespace)
                        .await
                        .with_context(|_| FetchOpaConfigMapSnafu {
                            configmap_name: configmap_name.clone(),
                            namespace: namespace.to_string(),
                        })?
                        .data
                        .and_then(|mut data| data.remove("OPA_SECRET_CLASS"));

                    NifiAuthorizationConfig::Opa {
                        configmap_name,
                        cache_entry_time_to_live_secs: opa_config.cache.entry_time_to_live.as_secs(),
                        cache_max_entries: opa_config.cache.max_entries,
                        secret_class,
                    }
                }
                None => NifiAuthorizationConfig::Default,
            },
            None => NifiAuthorizationConfig::Default,
        };

        Ok(config)
    }

    pub fn get_authorizers_config(
        &self,
        authentication_config: &NifiAuthenticationConfig,
    ) -> Result<String, Error> {
        let mut authorizers_xml = indoc! {r#"
            <?xml version="1.0" encoding="UTF-8" standalone="yes"?>
            <authorizers>
        "#}
        .to_string();

        match self {
            NifiAuthorizationConfig::Opa {
                cache_entry_time_to_live_secs,
                cache_max_entries,
                ..
            } => {
                authorizers_xml.push_str(&formatdoc! {r#"
                    <authorizer>
                        <identifier>authorizer</identifier>
                        <class>org.nifiopa.nifiopa.OpaAuthorizer</class>
                        <property name="CACHE_TIME_SECS">{cache_entry_time_to_live_secs}</property>
                        <property name="CACHE_MAX_ENTRY_COUNT">{cache_max_entries}</property>
                        <property name="OPA_URI">${{env:OPA_BASE_URL}}</property>
                        <property name="OPA_RULE_HEAD">nifi/allow</property>
                    </authorizer>
                "#});
            }
            NifiAuthorizationConfig::Default => match authentication_config {
                NifiAuthenticationConfig::SingleUser { .. }
                | NifiAuthenticationConfig::Oidc { .. } => {
                    authorizers_xml.push_str(indoc! {r#"
                            <authorizer>
                                <identifier>authorizer</identifier>
                                <class>org.apache.nifi.authorization.single.user.SingleUserAuthorizer</class>
                            </authorizer>
                        "#});
                }
                NifiAuthenticationConfig::Ldap { provider } => {
                    authorizers_xml.push_str(&self.get_default_ldap_authorizer(provider)?);
                }
            },
        }

        authorizers_xml.push_str(indoc! {r#"
            </authorizers>
        "#});
        Ok(authorizers_xml)
    }

    fn get_default_ldap_authorizer(
        &self,
        ldap: &ldap::v1alpha1::AuthenticationProvider,
    ) -> Result<String, Error> {
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

    pub fn get_env_vars(&self) -> Vec<EnvVar> {
        match self {
            NifiAuthorizationConfig::Opa { configmap_name, .. } => {
                vec![EnvVar {
                    name: "OPA_BASE_URL".to_owned(),
                    value_from: Some(EnvVarSource {
                        config_map_key_ref: Some(ConfigMapKeySelector {
                            key: "OPA".to_owned(),
                            name: configmap_name.to_owned(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]
            }
            NifiAuthorizationConfig::Default => vec![],
        }
    }

    pub fn has_opa_tls(&self) -> bool {
        matches!(self, NifiAuthorizationConfig::Opa { secret_class: Some(_), .. })
    }
}
