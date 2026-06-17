//! Builder for `nifi.properties`.

use std::collections::BTreeMap;

use snafu::{ResultExt, ensure};
use stackable_operator::memory::MemoryQuantity;

use super::{ConfigFileName, format_properties};
use crate::{
    controller::{
        NifiRoleGroupConfig, ValidatedCluster,
        build::{
            CalculateStorageQuotaSnafu, Error, GenerateOidcConfigSnafu, HTTPS_PORT,
            Nifi1RequiresZookeeperSnafu, PROTOCOL_PORT,
        },
    },
    crd::{
        constants::{
            NIFI_CONFIG_DIRECTORY, NIFI_PYTHON_EXTENSIONS_DIRECTORY,
            NIFI_PYTHON_FRAMEWORK_DIRECTORY, NIFI_PYTHON_WORKING_DIRECTORY,
        },
        storage::NifiRepository,
        v1alpha1,
    },
    security::{
        authentication::{
            NifiAuthenticationConfig, STACKABLE_SERVER_TLS_DIR, STACKABLE_TLS_STORE_PASSWORD,
        },
        oidc::add_oidc_config_to_properties,
    },
};

const STORAGE_PROVENANCE_UTILIZATION_FACTOR: f32 = 0.9;
const STORAGE_FLOW_ARCHIVE_UTILIZATION_FACTOR: f32 = 0.9;
const STORAGE_CONTENT_ARCHIVE_UTILIZATION_FACTOR: f32 = 0.5;

pub fn build(
    cluster: &ValidatedCluster,
    rg: &NifiRoleGroupConfig,
    proxy_hosts: &str,
) -> Result<String, Error> {
    let git_sync_resources = &rg.config.git_sync_resources;
    let product_version = &cluster.image.product_version;
    let auth_config = &cluster.cluster_config.authentication;
    let resource_config = &rg.config.resources;

    // TODO: Remove once we dropped support for all NiFi 1.x versions
    let is_nifi_1 = product_version.starts_with("1.");

    let mut properties = BTreeMap::new();
    // Core Properties
    // According to https://cwiki.apache.org/confluence/display/NIFI/Migration+Guidance#MigrationGuidance-Migratingto2.0.0-M1
    // The nifi.flow.configuration.file property in nifi.properties must be changed to reference
    // "flow.json.gz" instead of "flow.xml.gz"
    // TODO: Remove once we dropped support for all 1.x.x versions
    // TODO(malte): In order to use CLI tools like: ./bin/nifi.sh set-sensitive-properties-algorithm NIFI_PBKDF2_AES_GCM_256
    // we have to set both "nifi.flow.configuration.file" and "nifi.flow.configuration.json.file" in NiFi 1.x.x.
    if is_nifi_1 {
        properties.insert(
            "nifi.flow.configuration.file".to_string(),
            NifiRepository::Database.mount_path() + "/flow.xml.gz",
        );
        properties.insert(
            "nifi.flow.configuration.json.file".to_string(),
            NifiRepository::Database.mount_path() + "/flow.json.gz",
        );
    } else {
        properties.insert(
            "nifi.flow.configuration.file".to_string(),
            NifiRepository::Database.mount_path() + "/flow.json.gz",
        );
    }

    properties.insert(
        "nifi.flow.configuration.archive.enabled".to_string(),
        "true".to_string(),
    );
    properties.insert(
        "nifi.flow.configuration.archive.dir".to_string(),
        format!("{NIFI_CONFIG_DIRECTORY}/archive/"),
    );
    properties.insert(
        "nifi.flow.configuration.archive.max.time".to_string(),
        "".to_string(),
    );
    if let Some(capacity) = resource_config.storage.flowfile_repo.capacity.as_ref() {
        properties.insert(
            "nifi.flow.configuration.archive.max.storage".to_string(),
            storage_quantity_to_nifi(
                MemoryQuantity::try_from(capacity).context(CalculateStorageQuotaSnafu {
                    repo: NifiRepository::Flowfile,
                })? * STORAGE_FLOW_ARCHIVE_UTILIZATION_FACTOR,
            ),
        );
    }
    properties.insert(
        "nifi.flow.configuration.archive.max.count".to_string(),
        "".to_string(),
    );
    properties.insert(
        "nifi.flowcontroller.autoResumeState".to_string(),
        "true".to_string(),
    );
    properties.insert(
        "nifi.flowcontroller.graceful.shutdown.period".to_string(),
        "10 sec".to_string(),
    );
    properties.insert(
        "nifi.flowservice.writedelay.interval".to_string(),
        "500 ms".to_string(),
    );
    properties.insert(
        "nifi.administrative.yield.duration".to_string(),
        "30 sec".to_string(),
    );

    properties.insert(
        "nifi.authorizer.configuration.file".to_string(),
        format!("{NIFI_CONFIG_DIRECTORY}/{}", ConfigFileName::Authorizers),
    );
    properties.insert(
        "nifi.login.identity.provider.configuration.file".to_string(),
        format!(
            "{NIFI_CONFIG_DIRECTORY}/{}",
            ConfigFileName::LoginIdentityProviders
        ),
    );
    properties.insert(
        "nifi.templates.directory".to_string(),
        "./conf/templates".to_string(),
    );
    properties.insert("nifi.ui.banner.text".to_string(), "".to_string());
    properties.insert(
        "nifi.ui.autorefresh.interval".to_string(),
        "30 sec".to_string(),
    );
    properties.insert(
        "nifi.nar.library.directory".to_string(),
        "./lib".to_string(),
    );
    properties.insert(
        "nifi.nar.library.autoload.directory".to_string(),
        "./extensions".to_string(),
    );
    properties.insert(
        "nifi.nar.working.directory".to_string(),
        "./work/nar/".to_string(),
    );
    properties.insert(
        "nifi.documentation.working.directory".to_string(),
        "./work/docs/components".to_string(),
    );

    //###################
    // State Management #
    //###################
    properties.insert(
        "nifi.state.management.configuration.file".to_string(),
        "./conf/state-management.xml".to_string(),
    );
    // The ID of the local state provider
    properties.insert(
        "nifi.state.management.provider.local".to_string(),
        "local-provider".to_string(),
    );
    // The ID of the cluster-wide state provider. This will be ignored if NiFi is not clustered but must be populated if running in a cluster.
    properties.insert(
        "nifi.state.management.provider.cluster".to_string(),
        match cluster.cluster_config.clustering_backend {
            v1alpha1::NifiClusteringBackend::ZooKeeper { .. } => "zk-provider".to_string(),
            v1alpha1::NifiClusteringBackend::Kubernetes { .. } => "kubernetes-provider".to_string(),
        },
    );
    // Specifies whether or not this instance of NiFi should run an embedded ZooKeeper server
    properties.insert(
        "nifi.state.management.embedded.zookeeper.start".to_string(),
        "false".to_string(),
    );

    // H2 Settings
    properties.insert(
        "nifi.database.directory".to_string(),
        NifiRepository::Database.mount_path(),
    );
    properties.insert(
        "nifi.h2.url.append".to_string(),
        ";LOCK_TIMEOUT=25000;WRITE_DELAY=0;AUTO_SERVER=FALSE".to_string(),
    );

    // FlowFile Repository
    properties.insert(
        "nifi.flowfile.repository.implementation".to_string(),
        "org.apache.nifi.controller.repository.WriteAheadFlowFileRepository".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.wal.implementation".to_string(),
        "org.apache.nifi.wali.SequentialAccessWriteAheadLog".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.directory".to_string(),
        NifiRepository::Flowfile.mount_path(),
    );
    properties.insert(
        "nifi.flowfile.repository.checkpoint.interval".to_string(),
        "20 secs".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.always.sync".to_string(),
        "false".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.retain.orphaned.flowfiles".to_string(),
        "true".to_string(),
    );

    properties.insert(
        "nifi.swap.manager.implementation".to_string(),
        "org.apache.nifi.controller.FileSystemSwapManager".to_string(),
    );
    properties.insert("nifi.queue.swap.threshold".to_string(), "20000".to_string());

    // Content Repository
    properties.insert(
        "nifi.content.repository.implementation".to_string(),
        "org.apache.nifi.controller.repository.FileSystemRepository".to_string(),
    );
    properties.insert(
        "nifi.content.claim.max.appendable.size".to_string(),
        "1 MB".to_string(),
    );
    properties.insert(
        "nifi.content.repository.directory.default".to_string(),
        NifiRepository::Content.mount_path(),
    );
    // Cap archived content age so the archive directory stays bounded in
    // file count. NiFi treats empty as Long.MAX_VALUE, leaving size-based
    // purge as the only trigger; that lets the archive grow to whatever
    // half the PVC holds, and the startup directory scan in
    // FileSystemRepository.initializeRepository scales with file count.
    // 3 days covers a Friday-incident-investigated-Monday window for
    // content replay; users with longer requirements can extend via
    // configOverrides. The percentage-based threshold below acts as a
    // safety net if write rate outpaces time-based purge.
    // Also see https://github.com/stackabletech/nifi-operator/issues/354
    properties.insert(
        "nifi.content.repository.archive.max.retention.period".to_string(),
        "3 days".to_string(),
    );
    properties.insert(
        "nifi.content.repository.archive.max.usage.percentage".to_string(),
        format!("{}%", STORAGE_CONTENT_ARCHIVE_UTILIZATION_FACTOR * 100.0),
    );
    properties.insert(
        "nifi.content.repository.archive.enabled".to_string(),
        "true".to_string(),
    );
    properties.insert(
        "nifi.content.repository.always.sync".to_string(),
        "false".to_string(),
    );
    properties.insert(
        "nifi.content.viewer.url".to_string(),
        "../nifi-content-viewer/".to_string(),
    );

    // Provenance Repository Properties
    properties.insert(
        "nifi.provenance.repository.implementation".to_string(),
        "org.apache.nifi.provenance.WriteAheadProvenanceRepository".to_string(),
    );

    // Persistent Provenance Repository Properties
    properties.insert(
        "nifi.provenance.repository.directory.default".to_string(),
        NifiRepository::Provenance.mount_path(),
    );
    properties.insert(
        "nifi.provenance.repository.max.storage.time".to_string(),
        "".to_string(),
    );
    if let Some(capacity) = resource_config.storage.provenance_repo.capacity.as_ref() {
        properties.insert(
            "nifi.provenance.repository.max.storage.size".to_string(),
            storage_quantity_to_nifi(
                MemoryQuantity::try_from(capacity).context(CalculateStorageQuotaSnafu {
                    repo: NifiRepository::Provenance,
                })? * STORAGE_PROVENANCE_UTILIZATION_FACTOR,
            ),
        );
    }
    properties.insert(
        "nifi.provenance.repository.rollover.time".to_string(),
        "10 mins".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.rollover.size".to_string(),
        "100 MB".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.query.threads".to_string(),
        "2".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.index.threads".to_string(),
        "2".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.compress.on.rollover".to_string(),
        "true".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.always.sync".to_string(),
        "false".to_string(),
    );
    // Comma-separated list of fields. Fields that are not indexed will not be searchable. Valid fields are:
    // EventType, FlowFileUUID, Filename, TransitURI, ProcessorID, AlternateIdentifierURI, Relationship, Details
    properties.insert(
        "nifi.provenance.repository.indexed.fields".to_string(),
        "EventType, FlowFileUUID, Filename, ProcessorID, Relationship".to_string(),
    );
    // FlowFile Attributes that should be indexed and made searchable.  Some examples to consider are filename, uuid, mime.type
    properties.insert(
        "nifi.provenance.repository.indexed.attributes".to_string(),
        "".to_string(),
    );
    // Large values for the shard size will result in more Java heap usage when searching the Provenance Repository
    // but should provide better performance
    properties.insert(
        "nifi.provenance.repository.index.shard.size".to_string(),
        "500 MB".to_string(),
    );
    // Indicates the maximum length that a FlowFile attribute can be when retrieving a Provenance Event from
    // the repository. If the length of any attribute exceeds this value, it will be truncated when the event is retrieved.
    properties.insert(
        "nifi.provenance.repository.max.attribute.length".to_string(),
        "65536".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.concurrent.merge.threads".to_string(),
        "2".to_string(),
    );

    // Volatile Provenance Repository Properties
    properties.insert(
        "nifi.provenance.repository.buffer.size".to_string(),
        "100000".to_string(),
    );

    // Component Status Repository
    properties.insert(
        "nifi.components.status.repository.implementation".to_string(),
        "org.apache.nifi.controller.status.history.VolatileComponentStatusRepository".to_string(),
    );
    properties.insert(
        "nifi.components.status.repository.buffer.size".to_string(),
        "1440".to_string(),
    );
    properties.insert(
        "nifi.components.status.snapshot.frequency".to_string(),
        "1 min".to_string(),
    );

    // QuestDB Status History Repository Properties
    properties.insert(
        "nifi.status.repository.questdb.persist.node.days".to_string(),
        "14".to_string(),
    );
    properties.insert(
        "nifi.status.repository.questdb.persist.component.days".to_string(),
        "3".to_string(),
    );
    properties.insert(
        "nifi.status.repository.questdb.persist.location".to_string(),
        "./status_repository".to_string(),
    );

    //#############################################
    properties.insert(
        "nifi.web.https.host".to_string(),
        "${env:NODE_ADDRESS}".to_string(),
    );
    properties.insert("nifi.web.https.port".to_string(), HTTPS_PORT.to_string());
    properties.insert(
        "nifi.web.https.network.interface.default".to_string(),
        "".to_string(),
    );
    // Specifically listen on eth0 and lo interfaces.
    // Listening on lo allows k8s port-forward to work.
    // Once we listen on lo, we need to explicitly listen on eth0 so the server can be exposed (including health probes).
    // NOTE: We assume "eth0" is always the external interface in containers launched in Kubernetes.
    // It is possible that some container runtime will name it differently, but we haven't yet observed that.
    properties.insert(
        "nifi.web.https.network.interface.eth0".to_string(),
        "eth0".to_string(),
    );
    properties.insert(
        "nifi.web.https.network.interface.lo".to_string(),
        "lo".to_string(),
    );
    //#############################################
    properties.insert(
        "nifi.web.jetty.working.directory".to_string(),
        "./work/jetty".to_string(),
    );
    properties.insert("nifi.web.jetty.threads".to_string(), "200".to_string());
    properties.insert("nifi.web.max.header.size".to_string(), "16 KB".to_string());
    properties.insert("nifi.web.proxy.context.path".to_string(), "".to_string());
    properties.insert("nifi.web.proxy.host".to_string(), proxy_hosts.to_string());

    properties.insert(
        "nifi.sensitive.props.key".to_string(),
        "${file:UTF-8:/stackable/sensitiveproperty/nifiSensitivePropsKey}".to_string(),
    );
    properties.insert(
        "nifi.sensitive.props.key.protected".to_string(),
        "".to_string(),
    );

    // The algorithm has already been validated in the validate step (check_for_nifi_version).
    properties.insert(
        "nifi.sensitive.props.algorithm".to_string(),
        cluster
            .cluster_config
            .sensitive_properties_algorithm
            .to_string(),
    );

    // key and trust store
    // these properties are ok to hard code here, because the cannot be configured and are
    // generated with fixed values in the init container
    properties.insert(
        "nifi.security.keystore".to_string(),
        format!(
            "{keystore_path}/keystore.p12",
            keystore_path = STACKABLE_SERVER_TLS_DIR
        ),
    );
    properties.insert(
        "nifi.security.keystoreType".to_string(),
        "PKCS12".to_string(),
    );
    properties.insert(
        "nifi.security.keystorePasswd".to_string(),
        STACKABLE_TLS_STORE_PASSWORD.to_string(),
    );
    properties.insert(
        "nifi.security.truststore".to_string(),
        format!(
            "{keystore_path}/truststore.p12",
            keystore_path = STACKABLE_SERVER_TLS_DIR
        ),
    );
    properties.insert(
        "nifi.security.truststoreType".to_string(),
        "PKCS12".to_string(),
    );
    properties.insert(
        "nifi.security.truststorePasswd".to_string(),
        STACKABLE_TLS_STORE_PASSWORD.to_string(),
    );
    properties.insert(
        "nifi.security.user.login.identity.provider".to_string(),
        "login-identity-provider".to_string(),
    );
    properties.insert(
        "nifi.security.user.authorizer".to_string(),
        "authorizer".to_string(),
    );
    properties.insert(
        "nifi.security.allow.anonymous.authentication".to_string(),
        "false".to_string(),
    );
    properties.insert(
        "nifi.cluster.protocol.is.secure".to_string(),
        "true".to_string(),
    );

    if let NifiAuthenticationConfig::Oidc { provider, oidc, .. } = auth_config {
        add_oidc_config_to_properties(provider, oidc, &mut properties)
            .context(GenerateOidcConfigSnafu)?;
    };

    // cluster node properties (only configure for cluster nodes)
    properties.insert("nifi.cluster.is.node".to_string(), "true".to_string());
    properties.insert(
        "nifi.cluster.node.address".to_string(),
        "${env:NODE_ADDRESS}".to_string(),
    );
    properties.insert(
        "nifi.cluster.node.protocol.port".to_string(),
        PROTOCOL_PORT.to_string(),
    );
    properties.insert(
        "nifi.cluster.flow.election.max.candidates".to_string(),
        "".to_string(),
    );

    match cluster.cluster_config.clustering_backend {
        v1alpha1::NifiClusteringBackend::ZooKeeper { .. } => {
            properties.insert(
                "nifi.cluster.leader.election.implementation".to_string(),
                "CuratorLeaderElectionManager".to_string(),
            );

            // this will be replaced via a container command script
            properties.insert(
                "nifi.zookeeper.connect.string".to_string(),
                "${env:ZOOKEEPER_HOSTS}".to_string(),
            );

            // this will be replaced via a container command script
            properties.insert(
                "nifi.zookeeper.root.node".to_string(),
                "${env:ZOOKEEPER_CHROOT}".to_string(),
            );
        }

        v1alpha1::NifiClusteringBackend::Kubernetes {} => {
            ensure!(!is_nifi_1, Nifi1RequiresZookeeperSnafu);

            properties.insert(
                "nifi.cluster.leader.election.implementation".to_string(),
                "KubernetesLeaderElectionManager".to_string(),
            );

            // this will be replaced via a container command script
            properties.insert(
                "nifi.cluster.leader.election.kubernetes.lease.prefix".to_string(),
                "${env:STACKLET_NAME}".to_string(),
            );
        }
    }

    //####################
    // Custom components #
    //####################
    // NiFi 1.x does not support Python components and the Python configuration below is just
    // ignored.

    // The command used to launch Python.
    // This property must be set to enable Python-based processors.
    properties.insert("nifi.python.command".to_string(), "python3".to_string());

    // The directory that contains the Python framework for communicating between the Python and
    // Java processes.
    properties.insert(
        "nifi.python.framework.source.directory".to_string(),
        NIFI_PYTHON_FRAMEWORK_DIRECTORY.to_string(),
    );

    // The working directory where NiFi should store artifacts;
    // This property defaults to ./work/python but if you want to mount an emptyDir for the working
    // directory then another directory has to be set to avoid ownership conflicts with ./work/nar.
    properties.insert(
        "nifi.python.working.directory".to_string(),
        NIFI_PYTHON_WORKING_DIRECTORY.to_string(),
    );

    // The default directory that NiFi should look in to find custom Python-based components.
    // This directory is mentioned in the documentation
    // (docs/modules/nifi/pages/usage_guide/custom-components.adoc), so do not change it!
    properties.insert(
        "nifi.python.extensions.source.directory.default".to_string(),
        NIFI_PYTHON_EXTENSIONS_DIRECTORY.to_string(),
    );

    for (i, git_folder) in git_sync_resources
        .git_content_folders_as_string()
        .into_iter()
        .enumerate()
    {
        // The directory that NiFi should look in to find custom Python-based components.
        properties.insert(
            format!("nifi.python.extensions.source.directory.{i}"),
            git_folder.clone(),
        );

        // The directory that NiFi should look in to find custom Java-based components.
        properties.insert(format!("nifi.nar.library.directory.{i}"), git_folder);
    }
    //##########################

    // override with config overrides
    properties.extend(rg.config_overrides.nifi_properties.overrides.clone());

    Ok(format_properties(properties))
}

fn storage_quantity_to_nifi(quantity: MemoryQuantity) -> String {
    format!(
        "{}MB",
        quantity
            .scale_to(stackable_operator::memory::BinaryMultiple::Mebi)
            .value
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::build::{
        HTTPS_PORT,
        properties::test_support::{default_rg, minimal_validated_cluster},
    };

    /// Verify that core stable keys are present in the rendered nifi.properties with their
    /// expected values.  Assertions are on substrings — they do NOT assert the full file.
    #[test]
    fn test_stable_keys_present() {
        let cluster = minimal_validated_cluster();
        let rg = default_rg(&cluster);

        let props = build(&cluster, rg, "*").expect("build should succeed");

        // HTTPS port
        assert!(
            props.contains(&format!("nifi.web.https.port={HTTPS_PORT}")),
            "expected nifi.web.https.port={HTTPS_PORT} in output, got:\n{props}"
        );

        // Clustering enabled
        assert!(
            props.contains("nifi.cluster.is.node=true"),
            "expected nifi.cluster.is.node=true in output"
        );

        // Kubernetes clustering backend sets the Kubernetes election implementation
        assert!(
            props.contains(
                "nifi.cluster.leader.election.implementation=KubernetesLeaderElectionManager"
            ),
            "expected KubernetesLeaderElectionManager in output"
        );

        // Sensitive-properties algorithm default (NifiArgon2AesGcm256)
        assert!(
            props.contains("nifi.sensitive.props.algorithm=NIFI_ARGON2_AES_GCM_256"),
            "expected default algorithm NIFI_ARGON2_AES_GCM_256 in output"
        );

        // Proxy hosts wildcard from allow_all
        assert!(
            props.contains("nifi.web.proxy.host=*"),
            "expected nifi.web.proxy.host=* in output"
        );
    }

    /// Verify that a user configOverride for `nifi.properties` flows through to the output.
    #[test]
    fn test_config_override_wins() {
        use stackable_operator::v2::types::operator::RoleGroupName;

        use crate::{
            controller::validate::{build_role_group_configs, test_resolved_product_image},
            crd::{NifiRole, v1alpha1},
        };

        let yaml = r#"
            apiVersion: nifi.stackable.tech/v1alpha1
            kind: NifiCluster
            metadata:
              name: simple-nifi
              namespace: default
            spec:
              image:
                productVersion: 2.9.0
              clusterConfig:
                authentication:
                  - authenticationClass: nifi-admin-credentials-simple
                sensitiveProperties:
                  keySecret: simple-nifi-sensitive-property-key
                  autoGenerate: true
              nodes:
                roleGroups:
                  default:
                    replicas: 1
                    configOverrides:
                      nifi.properties:
                        some.custom.key: some-custom-value
        "#;
        let nifi: v1alpha1::NifiCluster = serde_yaml::from_str(yaml).expect("invalid test YAML");
        let mut role_group_configs =
            build_role_group_configs(&nifi, &test_resolved_product_image(), &None)
                .expect("failed to build role group configs");
        let default_rg_name = "default"
            .parse::<RoleGroupName>()
            .expect("valid role-group name");
        let rg = role_group_configs
            .get_mut(&NifiRole::Node)
            .and_then(|groups| groups.remove(&default_rg_name))
            .expect("default role group must exist");

        let mut cluster = minimal_validated_cluster();
        cluster
            .role_group_configs
            .get_mut(&NifiRole::Node)
            .unwrap()
            .insert(default_rg_name.clone(), rg);
        let rg = cluster
            .role_group_configs
            .get(&NifiRole::Node)
            .and_then(|groups| groups.get(&default_rg_name))
            .expect("default role group must exist");

        let props = build(&cluster, rg, "*").expect("build with override should succeed");

        assert!(
            props.contains("some.custom.key=some-custom-value"),
            "expected user override some.custom.key=some-custom-value to appear in output"
        );
        // The HTTPS port should still be present
        assert!(props.contains(&format!("nifi.web.https.port={HTTPS_PORT}")));
    }
}
