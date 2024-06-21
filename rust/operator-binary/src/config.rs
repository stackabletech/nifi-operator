use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write,
};

use product_config::{types::PropertyNameKind, ProductConfigManager};
use snafu::{ResultExt, Snafu};
use stackable_nifi_crd::{
    NifiCluster, NifiConfig, NifiConfigFragment, NifiRole, NifiSpec, NifiStorageConfig, HTTPS_PORT,
    PROTOCOL_PORT,
};
use stackable_operator::{
    commons::resources::Resources,
    memory::{BinaryMultiple, MemoryQuantity},
    product_config_utils::{
        transform_all_roles_to_config, validate_all_roles_and_groups_config,
        ValidatedRoleConfigByPropertyKind,
    },
    role_utils::Role,
};
use strum::{Display, EnumIter};

use crate::{
    operations::graceful_shutdown::graceful_shutdown_config_properties,
    security::authentication::{STACKABLE_SERVER_TLS_DIR, STACKABLE_TLS_STORE_PASSWORD},
};

pub const NIFI_CONFIG_DIRECTORY: &str = "/stackable/nifi/conf";

pub const NIFI_BOOTSTRAP_CONF: &str = "bootstrap.conf";
pub const NIFI_PROPERTIES: &str = "nifi.properties";
pub const NIFI_STATE_MANAGEMENT_XML: &str = "state-management.xml";
pub const JVM_SECURITY_PROPERTIES_FILE: &str = "security.properties";

// Keep some overhead for NiFi volumes, since cleanup is an asynchronous process that can stall active jobs
const STORAGE_PROVENANCE_UTILIZATION_FACTOR: f32 = 0.9;
const STORAGE_FLOW_ARCHIVE_UTILIZATION_FACTOR: f32 = 0.9;
// Content archive only counts _old_ data, so we want to allow some space for active data as well
const STORAGE_CONTENT_ARCHIVE_UTILIZATION_FACTOR: f32 = 0.5;
// Part of memory resources allocated for Java heap
const JAVA_HEAP_FACTOR: f32 = 0.8;

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
    #[snafu(display("Invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::product_config_utils::Error,
    },
    #[snafu(display("Invalid memory config"))]
    InvalidMemoryConfig {
        source: stackable_operator::memory::Error,
    },
    #[snafu(display("Failed to transform product configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::Error,
    },
    #[snafu(display("failed to calculate storage quota for {repo} repository"))]
    CalculateStorageQuota {
        source: stackable_operator::memory::Error,
        repo: NifiRepository,
    },
}

/// Create the NiFi bootstrap.conf
pub fn build_bootstrap_conf(
    nifi_config: &NifiConfig,
    overrides: BTreeMap<String, String>,
) -> Result<String, Error> {
    let mut bootstrap = BTreeMap::new();
    // Java command to use when running NiFi
    bootstrap.insert("java".to_string(), "java".to_string());
    // Username to use when running NiFi. This value will be ignored on Windows.
    bootstrap.insert("run.as".to_string(), "".to_string());
    // Preserve shell environment while runnning as "run.as" user
    bootstrap.insert("preserve.environment".to_string(), "false".to_string());
    // Configure where NiFi's lib and conf directories live
    bootstrap.insert("lib.dir".to_string(), "./lib".to_string());
    bootstrap.insert("conf.dir".to_string(), "./conf".to_string());
    bootstrap.extend(graceful_shutdown_config_properties(nifi_config));

    let mut java_args = Vec::with_capacity(18);
    // Disable JSR 199 so that we can use JSP's without running a JDK
    java_args.push("-Dorg.apache.jasper.compiler.disablejsr199=true".to_string());

    // Read memory limits from config
    if let Some(heap_size_definition) = &nifi_config.resources.memory.limit {
        tracing::debug!("Read {:?} from crd as memory limit", heap_size_definition);

        let heap_size = MemoryQuantity::try_from(heap_size_definition)
            .context(InvalidMemoryConfigSnafu)?
            .scale_to(BinaryMultiple::Mebi)
            * JAVA_HEAP_FACTOR;

        let java_heap = heap_size
            .format_for_java()
            .context(InvalidMemoryConfigSnafu)?;

        tracing::debug!(
            "Converted {:?} to {} for java heap config",
            &heap_size_definition,
            java_heap
        );
        // Push heap size config as max and min size to java args
        java_args.push(format!("-Xmx{}", java_heap));
        java_args.push(format!("-Xms{}", java_heap));
    } else {
        tracing::debug!("No memory limits defined");
    }

    java_args.push("-Djava.net.preferIPv4Stack=true".to_string());

    // allowRestrictedHeaders is required for Cluster/Node communications to work properly
    java_args.push("-Dsun.net.http.allowRestrictedHeaders=true".to_string());
    java_args.push("-Djava.protocol.handler.pkgs=sun.net.www.protocol".to_string());

    // The G1GC is known to cause some problems in Java 8 and earlier, but the issues were addressed in Java 9. If using Java 8 or earlier,
    // it is recommended that G1GC not be used, especially in conjunction with the Write Ahead Provenance Repository. However, if using a newer
    // version of Java, it can result in better performance without significant \"stop-the-world\" delays.
    java_args.push("-XX:+UseG1GC".to_string());

    // Set headless mode by default
    java_args.push("-Djava.awt.headless=true".to_string());
    // Root key in hexadecimal format for encrypted sensitive configuration values
    //bootstrap.insert("nifi.bootstrap.sensitive.key=".to_string(), "".to_string());
    // Sets the provider of SecureRandom to /dev/urandom to prevent blocking on VMs
    java_args.push("-Djava.security.egd=file:/dev/urandom".to_string());
    // Requires JAAS to use only the provided JAAS configuration to authenticate a Subject, without using any "fallback" methods (such as prompting for username/password)
    // Please see https://docs.oracle.com/javase/8/docs/technotes/guides/security/jgss/single-signon.html, section "EXCEPTIONS TO THE MODEL"
    java_args.push("-Djavax.security.auth.useSubjectCredsOnly=true".to_string());

    // Zookeeper 3.5 now includes an Admin Server that starts on port 8080, since NiFi is already using that port disable by default.
    // Please see https://zookeeper.apache.org/doc/current/zookeeperAdmin.html#sc_adminserver_config for configuration options.
    java_args.push("-Dzookeeper.admin.enableServer=false".to_string());

    // JVM security properties include especially TTL values for the positive and negative DNS caches.
    java_args.push(format!(
        "-Djava.security.properties={NIFI_CONFIG_DIRECTORY}/{JVM_SECURITY_PROPERTIES_FILE}"
    ));

    // add java args
    bootstrap.extend(
        java_args
            .into_iter()
            .enumerate()
            .map(|(i, a)| (format!("java.arg.{}", i + 1), a)),
    );
    // override with config overrides
    bootstrap.extend(overrides);

    Ok(format_properties(bootstrap))
}

/// Create the NiFi nifi.properties
pub fn build_nifi_properties(
    spec: &NifiSpec,
    resource_config: &Resources<NifiStorageConfig>,
    proxy_hosts: &str,
    overrides: BTreeMap<String, String>,
) -> Result<String, Error> {
    let mut properties = BTreeMap::new();
    // Core Properties
    properties.insert(
        "nifi.flow.configuration.file".to_string(),
        NifiRepository::Database.mount_path() + "/flow.xml.gz",
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
        "zk-provider".to_string(),
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
    // zookeeper properties, used for cluster management
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

    // override with config overrides
    properties.extend(overrides);

    Ok(format_properties(properties))
}

pub fn build_state_management_xml() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>
        <stateManagement>
          <local-provider>
          <id>local-provider</id>
            <class>org.apache.nifi.controller.state.providers.local.WriteAheadLocalStateProvider</class>
            <property name=\"Directory\">{}</property>
            <property name=\"Always Sync\">false</property>
            <property name=\"Partitions\">16</property>
            <property name=\"Checkpoint Interval\">2 mins</property>
          </local-provider>
          <cluster-provider>
            <id>zk-provider</id>
            <class>org.apache.nifi.controller.state.providers.zookeeper.ZooKeeperStateProvider</class>
            <property name=\"Connect String\">${{env:ZOOKEEPER_HOSTS}}</property>
            <property name=\"Root Node\">${{env:ZOOKEEPER_CHROOT}}</property>
            <property name=\"Session Timeout\">10 seconds</property>
            <property name=\"Access Control\">Open</property>
          </cluster-provider>
        </stateManagement>",
        &NifiRepository::State.mount_path(),
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
    resource: &NifiCluster,
    version: &str,
    role: &Role<NifiConfigFragment>,
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
