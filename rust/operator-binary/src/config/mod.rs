use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write,
};

use jvm::build_merged_jvm_config;
use product_config::{ProductConfigManager, types::PropertyNameKind};
use snafu::{ResultExt, Snafu, ensure};
use stackable_operator::{
    commons::resources::Resources,
    crd::git_sync,
    memory::MemoryQuantity,
    product_config_utils::{
        ValidatedRoleConfigByPropertyKind, transform_all_roles_to_config,
        validate_all_roles_and_groups_config,
    },
    role_utils::{GenericRoleConfig, JavaCommonConfig, Role},
};
use strum::{Display, EnumIter};

use crate::{
    crd::{
        HTTPS_PORT, NifiConfig, NifiConfigFragment, NifiRole, NifiStorageConfig, PROTOCOL_PORT,
        v1alpha1::{self, NifiClusteringBackend},
    },
    operations::graceful_shutdown::graceful_shutdown_config_properties,
    security::{
        authentication::{
            NifiAuthenticationConfig, STACKABLE_SERVER_TLS_DIR, STACKABLE_TLS_STORE_PASSWORD,
        },
        oidc::{self, add_oidc_config_to_properties},
    },
};

pub mod jvm;

pub const NIFI_CONFIG_DIRECTORY: &str = "/stackable/nifi/conf";
pub const NIFI_PYTHON_WORKING_DIRECTORY: &str = "/nifi-python-working-directory";

pub const NIFI_BOOTSTRAP_CONF: &str = "bootstrap.conf";
pub const NIFI_PROPERTIES: &str = "nifi.properties";
pub const NIFI_STATE_MANAGEMENT_XML: &str = "state-management.xml";
pub const JVM_SECURITY_PROPERTIES_FILE: &str = "security.properties";

// Keep some overhead for NiFi volumes, since cleanup is an asynchronous process that can stall active jobs
const STORAGE_PROVENANCE_UTILIZATION_FACTOR: f32 = 0.9;
const STORAGE_FLOW_ARCHIVE_UTILIZATION_FACTOR: f32 = 0.9;
// Content archive only counts _old_ data, so we want to allow some space for active data as well
const STORAGE_CONTENT_ARCHIVE_UTILIZATION_FACTOR: f32 = 0.5;

#[derive(Debug, Display, EnumIter)]
pub enum NifiRepository {
    #[strum(serialize = "flowfile")]
    Flowfile,
    #[strum(serialize = "database")]
    Database,
    #[strum(serialize = "content")]
    Content,
    #[strum(serialize = "provenance")]
    Provenance,
    #[strum(serialize = "state")]
    State,
}

impl NifiRepository {
    pub fn repository(&self) -> String {
        format!("{}-repository", self)
    }

    pub fn mount_path(&self) -> String {
        format!("/stackable/data/{}", self)
    }
}

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::product_config_utils::Error,
    },

    #[snafu(display("invalid memory resource configuration - missing default or value in crd?"))]
    MissingMemoryResourceConfig,

    #[snafu(display("invalid JVM config"))]
    InvalidJVMConfig { source: jvm::Error },

    #[snafu(display("failed to transform product configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::Error,
    },

    #[snafu(display("failed to calculate storage quota for {repo} repository"))]
    CalculateStorageQuota {
        source: stackable_operator::memory::Error,
        repo: NifiRepository,
    },

    #[snafu(display("failed to generate OIDC config"))]
    GenerateOidcConfig { source: oidc::Error },

    #[snafu(display(
        "NiFi 1.x requires ZooKeeper (hint: upgrade to NiFi 2.x or set .spec.clusterConfig.zookeeperConfigMapName)"
    ))]
    Nifi1RequiresZookeeper,
}

/// Create the NiFi bootstrap.conf
pub fn build_bootstrap_conf(
    merged_config: &NifiConfig,
    overrides: BTreeMap<String, String>,
    role: &Role<NifiConfigFragment, GenericRoleConfig, JavaCommonConfig>,
    role_group: &str,
) -> Result<String, Error> {
    let mut bootstrap = BTreeMap::new();
    // Java command to use when running NiFi
    bootstrap.insert("java".to_string(), "java".to_string());
    // Username to use when running NiFi. This value will be ignored on Windows.
    bootstrap.insert("run.as".to_string(), "".to_string());
    // Preserve shell environment while running as "run.as" user
    bootstrap.insert("preserve.environment".to_string(), "false".to_string());
    // Configure where NiFi's lib and conf directories live
    bootstrap.insert("lib.dir".to_string(), "./lib".to_string());
    bootstrap.insert("conf.dir".to_string(), "./conf".to_string());
    bootstrap.extend(graceful_shutdown_config_properties(merged_config));

    let merged_jvm_config =
        build_merged_jvm_config(merged_config, role, role_group).context(InvalidJVMConfigSnafu)?;

    for (index, argument) in merged_jvm_config
        .effective_jvm_config_after_merging()
        .iter()
        .enumerate()
    {
        bootstrap.insert(format!("java.arg.{}", index + 1), argument.clone());
    }

    // configOverrides come last
    bootstrap.extend(overrides);

    Ok(format_properties(bootstrap))
}

/// Create the NiFi nifi.properties
pub fn build_nifi_properties(
    spec: &v1alpha1::NifiClusterSpec,
    resource_config: &Resources<NifiStorageConfig>,
    proxy_hosts: &str,
    auth_config: &NifiAuthenticationConfig,
    overrides: BTreeMap<String, String>,
    product_version: &str,
    git_sync_resources: &git_sync::v1alpha1::GitSyncResources,
) -> Result<String, Error> {
    // TODO: Remove once we dropped support for all NiFi 1.x versions
    let is_nifi_1 = product_version.starts_with("1.");

    let mut properties = BTreeMap::new();
    // Core Properties
    // According to https://cwiki.apache.org/confluence/display/NIFI/Migration+Guidance#MigrationGuidance-Migratingto2.0.0-M1
    // The nifi.flow.configuration.file property in nifi.properties must be changed to reference
    // "flow.json.gz" instead of "flow.xml.gz"
    let flow_file_name = if is_nifi_1 {
        "flow.xml.gz"
    } else {
        "flow.json.gz"
    };
    properties.insert(
        "nifi.flow.configuration.file".to_string(),
        NifiRepository::Database.mount_path() + "/" + flow_file_name,
    );
    properties.insert(
        "nifi.flow.configuration.archive.enabled".to_string(),
        "true".to_string(),
    );
    properties.insert(
        "nifi.flow.configuration.archive.dir".to_string(),
        "/stackable/nifi/conf/archive/".to_string(),
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
        "/stackable/nifi/conf/authorizers.xml".to_string(),
    );
    properties.insert(
        "nifi.login.identity.provider.configuration.file".to_string(),
        "/stackable/nifi/conf/login-identity-providers.xml".to_string(),
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
        match spec.cluster_config.clustering_backend {
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
    properties.insert(
        "nifi.content.repository.archive.max.retention.period".to_string(),
        "".to_string(),
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

    // Volatile Provenance Respository Properties
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

    let algorithm = &spec
        .cluster_config
        .sensitive_properties
        .algorithm
        .clone()
        .unwrap_or_default();
    properties.insert(
        "nifi.sensitive.props.algorithm".to_string(),
        algorithm.to_string(),
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
    // TODO: set to 1 min for testing (default 5)
    properties.insert(
        "nifi.cluster.flow.election.max.wait.time".to_string(),
        "1 mins".to_string(),
    );
    properties.insert(
        "nifi.cluster.flow.election.max.candidates".to_string(),
        "".to_string(),
    );

    match spec.cluster_config.clustering_backend {
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
        "/stackable/nifi/python/framework/".to_string(),
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
        "/stackable/nifi/python/extensions/".to_string(),
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
    properties.extend(overrides);

    Ok(format_properties(properties))
}

pub fn build_state_management_xml(clustering_backend: &NifiClusteringBackend) -> String {
    // Inert providers are ignored by NiFi itself, but templating still fails if they refer to invalid environment variables,
    // so only include the actually used provider.
    let cluster_provider = match clustering_backend {
        NifiClusteringBackend::ZooKeeper { .. } => {
            r#"<cluster-provider>
              <id>zk-provider</id>
              <class>org.apache.nifi.controller.state.providers.zookeeper.ZooKeeperStateProvider</class>
              <property name="Connect String">${env:ZOOKEEPER_HOSTS}</property>
              <property name="Root Node">${env:ZOOKEEPER_CHROOT}</property>
              <property name="Session Timeout">10 seconds</property>
              <property name="Access Control">Open</property>
            </cluster-provider>"#
        }
        NifiClusteringBackend::Kubernetes {} => {
            r#"<cluster-provider>
              <id>kubernetes-provider</id>
              <class>org.apache.nifi.kubernetes.state.provider.KubernetesConfigMapStateProvider</class>
              <property name="ConfigMap Name Prefix">${env:STACKLET_NAME}</property>
            </cluster-provider>"#
        }
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
        <stateManagement>
          <local-provider>
            <id>local-provider</id>
            <class>org.apache.nifi.controller.state.providers.local.WriteAheadLocalStateProvider</class>
            <property name="Directory">{local_state_path}</property>
            <property name="Always Sync">false</property>
            <property name="Partitions">16</property>
            <property name="Checkpoint Interval">2 mins</property>
          </local-provider>
          {cluster_provider}
        </stateManagement>"#,
        local_state_path = NifiRepository::State.mount_path(),
    )
}

/// Defines all required roles and their required configuration. In this case we need three files:
/// `bootstrap.conf`, `nifi.properties` and `state-management.xml`.
///
/// We do not require any env variables yet. We will however utilize them to change the
/// configuration directory - check <https://github.com/apache/nifi/pull/2985> for more detail.
///
/// The roles and their configs are then validated and complemented by the product config.
///
/// # Arguments
/// * `resource`        - The NifiCluster containing the role definitions.
/// * `version`         - The NifiCluster version.
/// * `product_config`  - The product config to validate and complement the user config.
///
pub fn validated_product_config(
    resource: &v1alpha1::NifiCluster,
    version: &str,
    role: &Role<NifiConfigFragment, GenericRoleConfig, JavaCommonConfig>,
    product_config: &ProductConfigManager,
) -> Result<ValidatedRoleConfigByPropertyKind, Error> {
    let mut roles = HashMap::new();
    roles.insert(
        NifiRole::Node.to_string(),
        (
            vec![
                PropertyNameKind::File(NIFI_BOOTSTRAP_CONF.to_string()),
                PropertyNameKind::File(NIFI_PROPERTIES.to_string()),
                PropertyNameKind::File(NIFI_STATE_MANAGEMENT_XML.to_string()),
                PropertyNameKind::File(JVM_SECURITY_PROPERTIES_FILE.to_string()),
                PropertyNameKind::Env,
            ],
            role.clone(),
        ),
    );

    let role_config =
        transform_all_roles_to_config(resource, roles).context(ProductConfigTransformSnafu)?;

    validate_all_roles_and_groups_config(version, &role_config, product_config, false, false)
        .context(InvalidProductConfigSnafu)
}

// TODO: Use crate like https://crates.io/crates/java-properties (currently does not work for Nifi
// because of escapes), to have save handling of escapes etc.
fn format_properties(properties: BTreeMap<String, String>) -> String {
    let mut result = String::new();

    for (key, value) in properties {
        let _ = writeln!(result, "{}={}", key, value);
    }

    result
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
    use indoc::indoc;

    use super::*;
    use crate::{config::build_bootstrap_conf, crd::v1alpha1};

    #[test]
    fn test_build_bootstrap_conf_defaults() {
        let input = r#"
        apiVersion: nifi.stackable.tech/v1alpha1
        kind: NifiCluster
        metadata:
          name: simple-nifi
        spec:
          image:
            productVersion: 1.27.0
          clusterConfig:
            authentication:
              - authenticationClass: nifi-admin-credentials-simple
            sensitiveProperties:
              keySecret: simple-nifi-sensitive-property-key
              autoGenerate: true
            zookeeperConfigMapName: simple-nifi-znode
          nodes:
            roleGroups:
              default:
                replicas: 1
        "#;
        let bootstrap_conf = construct_bootstrap_conf(input);

        assert_eq!(bootstrap_conf, indoc! {"
                conf.dir=./conf
                graceful.shutdown.seconds=300
                java=java
                java.arg.1=-Xmx3276m
                java.arg.10=-Djavax.security.auth.useSubjectCredsOnly=true
                java.arg.11=-Dzookeeper.admin.enableServer=false
                java.arg.12=-Djava.security.properties=/stackable/nifi/conf/security.properties
                java.arg.2=-Xms3276m
                java.arg.3=-XX:+UseG1GC
                java.arg.4=-Djava.awt.headless=true
                java.arg.5=-Dorg.apache.jasper.compiler.disablejsr199=true
                java.arg.6=-Djava.net.preferIPv4Stack=true
                java.arg.7=-Dsun.net.http.allowRestrictedHeaders=true
                java.arg.8=-Djava.protocol.handler.pkgs=sun.net.www.protocol
                java.arg.9=-Djava.security.egd=file:/dev/urandom
                lib.dir=./lib
                preserve.environment=false
                run.as=
            "});
    }

    #[test]
    fn test_build_bootstrap_conf_jvm_argument_overrides() {
        let input = r#"
        apiVersion: nifi.stackable.tech/v1alpha1
        kind: NifiCluster
        metadata:
          name: simple-nifi
        spec:
          image:
            productVersion: 1.27.0
          clusterConfig:
            authentication:
              - authenticationClass: nifi-admin-credentials-simple
            sensitiveProperties:
              keySecret: simple-nifi-sensitive-property-key
              autoGenerate: true
            zookeeperConfigMapName: simple-nifi-znode
          nodes:
            config:
              resources:
                memory:
                  limit: 42Gi
            jvmArgumentOverrides:
              remove:
                - -XX:+UseG1GC
              add:
                - -Dhttps.proxyHost=proxy.my.corp
                - -Dhttps.proxyPort=8080
                - -Djava.net.preferIPv4Stack=true
            roleGroups:
              default:
                replicas: 1
                jvmArgumentOverrides:
                  # We need more memory!
                  removeRegex:
                    - -Xmx.*
                    - -Dhttps.proxyPort=.*
                  add:
                    - -Xmx40000m
                    - -Dhttps.proxyPort=1234
        "#;
        let bootstrap_conf = construct_bootstrap_conf(input);

        assert_eq!(bootstrap_conf, indoc! {"
                conf.dir=./conf
                graceful.shutdown.seconds=300
                java=java
                java.arg.1=-Xms34406m
                java.arg.10=-Djava.security.properties=/stackable/nifi/conf/security.properties
                java.arg.11=-Dhttps.proxyHost=proxy.my.corp
                java.arg.12=-Djava.net.preferIPv4Stack=true
                java.arg.13=-Xmx40000m
                java.arg.14=-Dhttps.proxyPort=1234
                java.arg.2=-Djava.awt.headless=true
                java.arg.3=-Dorg.apache.jasper.compiler.disablejsr199=true
                java.arg.4=-Djava.net.preferIPv4Stack=true
                java.arg.5=-Dsun.net.http.allowRestrictedHeaders=true
                java.arg.6=-Djava.protocol.handler.pkgs=sun.net.www.protocol
                java.arg.7=-Djava.security.egd=file:/dev/urandom
                java.arg.8=-Djavax.security.auth.useSubjectCredsOnly=true
                java.arg.9=-Dzookeeper.admin.enableServer=false
                lib.dir=./lib
                preserve.environment=false
                run.as=
            "});
    }

    fn construct_bootstrap_conf(nifi_cluster: &str) -> String {
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::from_str(nifi_cluster).expect("illegal test input");

        let nifi_role = NifiRole::Node;
        let role = nifi.spec.nodes.as_ref().unwrap();
        let merged_config = nifi.merged_config(&nifi_role, "default").unwrap();

        build_bootstrap_conf(&merged_config, BTreeMap::new(), role, "default").unwrap()
    }
}
