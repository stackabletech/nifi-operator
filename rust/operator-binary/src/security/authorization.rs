use indoc::{formatdoc, indoc};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    client::Client,
    commons::opa::OpaConfig,
    crd::authentication::ldap,
    k8s_openapi::api::core::v1::{ConfigMap, ConfigMapKeySelector, EnvVar, EnvVarSource},
    kube::ResourceExt,
};

use super::authentication::NifiAuthenticationConfig;
use crate::crd::{
    authorization::{NifiAuthorization, NifiOpaConfig},
    v1alpha1,
};

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
        config: OpaConfig,
        cache_entry_time_to_live_secs: u64,
        cache_max_entries: u32,
        secret_class: Option<String>,
    },
    Default,
}

impl NifiAuthorizationConfig {
    pub async fn from(
        nifi_authorization: &NifiAuthorization,
        client: &Client,
        namespace: &str,
    ) -> Result<Self, Error> {
        let authz = match nifi_authorization {
            NifiAuthorization::Opa {
                opa: NifiOpaConfig { opa, cache },
            } => {
                // Resolve the secret class from the ConfigMap
                let secret_class = client
                    .get::<ConfigMap>(&opa.config_map_name, namespace)
                    .await
                    .with_context(|_| FetchOpaConfigMapSnafu {
                        configmap_name: &opa.config_map_name,
                        namespace,
                    })?
                    .data
                    .and_then(|mut data| data.remove("OPA_SECRET_CLASS"));

                NifiAuthorizationConfig::Opa {
                    config: opa.to_owned(),
                    cache_entry_time_to_live_secs: cache.entry_time_to_live.as_secs(),
                    cache_max_entries: cache.max_entries,
                    secret_class,
                }
            }
            NifiAuthorization::SingleUser {} => NifiAuthorizationConfig::Default,
            NifiAuthorization::Standard {} => NifiAuthorizationConfig::Default,
        };

        Ok(authz)
    }

    pub fn get_authorizers_config(
        &self,
        nifi_cluster: &v1alpha1::NifiCluster,
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
                config: OpaConfig { package, .. },
                ..
            } => {
                // According to [`OpaConfig::document_url`] we default the stacklet name
                let package = package.clone().unwrap_or_else(|| nifi_cluster.name_any());
                authorizers_xml.push_str(&formatdoc! {r#"
                    <authorizer>
                        <identifier>authorizer</identifier>
                        <class>org.nifiopa.nifiopa.OpaAuthorizer</class>
                        <property name="CACHE_TIME_SECS">{cache_entry_time_to_live_secs}</property>
                        <property name="CACHE_MAX_ENTRY_COUNT">{cache_max_entries}</property>
                        <property name="OPA_URI">${{env:OPA_BASE_URL}}</property>
                        <property name="OPA_RULE_HEAD">{package}/allow</property>
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
            NifiAuthorizationConfig::Opa {
                config: OpaConfig {
                    config_map_name, ..
                },
                ..
            } => {
                vec![EnvVar {
                    name: "OPA_BASE_URL".to_owned(),
                    value_from: Some(EnvVarSource {
                        config_map_key_ref: Some(ConfigMapKeySelector {
                            key: "OPA".to_owned(),
                            name: config_map_name.to_owned(),
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
        matches!(
            self,
            NifiAuthorizationConfig::Opa {
                secret_class: Some(_),
                ..
            }
        )
    }
}
