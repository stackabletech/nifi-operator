use std::collections::BTreeMap;

use indoc::{formatdoc, indoc};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::pod::volume::{SecretOperatorVolumeSourceBuilder, VolumeBuilder},
    client::Client,
    commons::opa::OpaConfig,
    k8s_openapi::{
        api::core::v1::{
            ConfigMap, ConfigMapKeySelector, EnvVar, EnvVarSource, PersistentVolumeClaim,
            PersistentVolumeClaimSpec, Volume, VolumeMount, VolumeResourceRequirements,
        },
        apimachinery::pkg::api::resource::Quantity,
    },
    kube::{ResourceExt, api::ObjectMeta},
};

use crate::{
    config::NIFI_PVC_STORAGE_DIRECTORY,
    crd::{
        authorization::{NifiAccessPolicyProvider, NifiAuthorization, NifiOpaConfig},
        v1alpha1,
    },
};

const OPA_TLS_VOLUME_NAME: &str = "opa-tls";
pub const OPA_TLS_MOUNT_PATH: &str = "/stackable/opa_tls";

const FILE_BASED_MOUNT_NAME: &str = "filebased";
const FILE_BASED_MOUNT_DIRECTORY: &str = "filebased";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("Failed to fetch OPA ConfigMap {configmap_name}"))]
    FetchOpaConfigMap {
        source: stackable_operator::client::Error,
        configmap_name: String,
        namespace: String,
    },
    #[snafu(display("failed to build OPA TLS certificate volume"))]
    OpaTlsCertSecretClassVolumeBuild {
        source: stackable_operator::builder::pod::volume::SecretOperatorVolumeSourceBuilderError,
    },
}

pub enum ResolvedNifiAuthorizationConfig {
    Opa {
        config: OpaConfig,
        cache_entry_time_to_live_secs: u64,
        cache_max_entries: u32,
        secret_class: Option<String>,
    },
    SingleUser,
    Standard {
        access_policy_provider: NifiAccessPolicyProvider,
    },
}

impl ResolvedNifiAuthorizationConfig {
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

                ResolvedNifiAuthorizationConfig::Opa {
                    config: opa.to_owned(),
                    cache_entry_time_to_live_secs: cache.entry_time_to_live.as_secs(),
                    cache_max_entries: cache.max_entries,
                    secret_class,
                }
            }
            NifiAuthorization::SingleUser {} => ResolvedNifiAuthorizationConfig::SingleUser,
            NifiAuthorization::Standard {
                access_policy_provider,
            } => ResolvedNifiAuthorizationConfig::Standard {
                access_policy_provider: access_policy_provider.to_owned(),
            },
        };

        Ok(authz)
    }

    pub fn get_authorizers_config(
        &self,
        nifi_cluster: &v1alpha1::NifiCluster,
    ) -> Result<String, Error> {
        let mut authorizers_xml = indoc! {r#"
            <?xml version="1.0" encoding="UTF-8" standalone="yes"?>
            <authorizers>
        "#}
        .to_string();

        match self {
            ResolvedNifiAuthorizationConfig::Opa {
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
            ResolvedNifiAuthorizationConfig::SingleUser => {
                authorizers_xml.push_str(indoc! {r#"
                    <authorizer>
                        <identifier>authorizer</identifier>
                        <class>org.apache.nifi.authorization.single.user.SingleUserAuthorizer</class>
                    </authorizer>
                "#});
            }
            ResolvedNifiAuthorizationConfig::Standard {
                access_policy_provider: NifiAccessPolicyProvider::FileBased { initial_admin_user },
            } => {
                let file_based_mount_path = Self::file_based_mount_path();

                authorizers_xml.push_str(&formatdoc! {r#"
                    <userGroupProvider>
                        <identifier>file-user-group-provider</identifier>
                        <class>org.apache.nifi.authorization.FileUserGroupProvider</class>
                        <property name="Users File">{file_based_mount_path}/users.xml</property>
                        <property name="Initial User Identity admin">{initial_admin_user}</property>

                        <!-- As the secret-operator provides the NiFi nodes with cert with a common name of "generated certificate for pod" we have to put that here -->
                        <property name="Initial User Identity other-nifis">CN=generated certificate for pod</property>
                    </userGroupProvider>

                    <accessPolicyProvider>
                        <identifier>file-access-policy-provider</identifier>
                        <class>org.apache.nifi.authorization.FileAccessPolicyProvider</class>
                        <property name="User Group Provider">file-user-group-provider</property>
                        <property name="Authorizations File">{file_based_mount_path}/authorizations.xml</property>
                        <property name="Initial Admin Identity">{initial_admin_user}</property>

                        <!-- As the secret-operator provides the NiFi nodes with cert with a common name of "generated certificate for pod" we have to put that here -->
                        <property name="Node Identity other-nifis">CN=generated certificate for pod</property>
                    </accessPolicyProvider>

                    <authorizer>
                        <identifier>authorizer</identifier>
                        <class>org.apache.nifi.authorization.StandardManagedAuthorizer</class>
                        <property name="Access Policy Provider">file-access-policy-provider</property>
                    </authorizer>
                "#});
            }
        }

        authorizers_xml.push_str(indoc! {r#"
            </authorizers>
        "#});

        Ok(authorizers_xml)
    }

    pub fn get_env_vars(&self) -> Vec<EnvVar> {
        match self {
            ResolvedNifiAuthorizationConfig::Opa {
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
            ResolvedNifiAuthorizationConfig::SingleUser => vec![],
            ResolvedNifiAuthorizationConfig::Standard { .. } => vec![],
        }
    }

    pub fn get_volume_mounts(&self) -> Vec<VolumeMount> {
        let mut volume_mounts = vec![];
        match self {
            ResolvedNifiAuthorizationConfig::Opa {
                secret_class: Some(_),
                ..
            } => volume_mounts.push(VolumeMount {
                name: OPA_TLS_VOLUME_NAME.into(),
                mount_path: OPA_TLS_MOUNT_PATH.into(),
                ..VolumeMount::default()
            }),
            ResolvedNifiAuthorizationConfig::Standard {
                access_policy_provider,
            } => {
                if matches!(
                    access_policy_provider,
                    NifiAccessPolicyProvider::FileBased {
                        initial_admin_user: _
                    }
                ) {
                    volume_mounts.push(VolumeMount {
                        name: FILE_BASED_MOUNT_NAME.into(),
                        mount_path: Self::file_based_mount_path(),
                        ..VolumeMount::default()
                    })
                }
            }
            _ => {}
        }

        volume_mounts
    }

    pub fn get_volumes(&self) -> Result<Vec<Volume>, Error> {
        let mut volumes = vec![];

        if let ResolvedNifiAuthorizationConfig::Opa {
            secret_class: Some(secret_class),
            ..
        } = self
        {
            volumes.push(
                VolumeBuilder::new(OPA_TLS_VOLUME_NAME)
                    .ephemeral(
                        SecretOperatorVolumeSourceBuilder::new(secret_class)
                            .build()
                            .context(OpaTlsCertSecretClassVolumeBuildSnafu)?,
                    )
                    .build(),
            )
        };

        Ok(volumes)
    }

    pub fn get_pvcs(&self) -> Vec<PersistentVolumeClaim> {
        let mut pvcs = vec![];

        if let ResolvedNifiAuthorizationConfig::Standard { .. } = self {
            pvcs.push(PersistentVolumeClaim {
                metadata: ObjectMeta {
                    name: Some(FILE_BASED_MOUNT_NAME.to_owned()),
                    ..ObjectMeta::default()
                },
                spec: Some(PersistentVolumeClaimSpec {
                    access_modes: Some(vec!["ReadWriteOnce".to_owned()]),
                    resources: Some(VolumeResourceRequirements {
                        requests: Some({
                            let mut map = BTreeMap::new();
                            map.insert("storage".to_string(), Quantity("16Mi".to_owned()));
                            map
                        }),
                        ..Default::default()
                    }),
                    ..PersistentVolumeClaimSpec::default()
                }),
                ..PersistentVolumeClaim::default()
            })
        };

        pvcs
    }

    pub fn has_opa_tls(&self) -> bool {
        matches!(
            self,
            ResolvedNifiAuthorizationConfig::Opa {
                secret_class: Some(_),
                ..
            }
        )
    }

    fn file_based_mount_path() -> String {
        format!("{NIFI_PVC_STORAGE_DIRECTORY}/{FILE_BASED_MOUNT_DIRECTORY}")
    }
}
