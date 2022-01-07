use snafu::{ResultExt, Snafu};
use stackable_nifi_crd::{NifiCluster, NifiConfig, NifiRole, NifiSpec, HTTPS_PORT, PROTOCOL_PORT};
use stackable_operator::product_config::types::PropertyNameKind;
use stackable_operator::product_config::ProductConfigManager;
use stackable_operator::product_config_utils::{
    transform_all_roles_to_config, validate_all_roles_and_groups_config,
    ValidatedRoleConfigByPropertyKind,
};
use stackable_operator::role_utils::Role;
use std::collections::{BTreeMap, HashMap};
use strum_macros::Display;
use strum_macros::EnumIter;

pub const NIFI_BOOTSTRAP_CONF: &str = "bootstrap.conf";
pub const NIFI_PROPERTIES: &str = "nifi.properties";
pub const NIFI_STATE_MANAGEMENT_XML: &str = "state-management.xml";

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
}

impl NifiRepository {
    pub fn repository(&self) -> String {
        format!("{}-repository", self.to_string())
    }

    pub fn mount_path(&self) -> String {
        format!("/stackable/data/{}", self.to_string())
    }
}

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("Invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("Failed to transform product configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::ConfigError,
    },
}

/// Create the NiFi bootstrap.conf
pub fn build_bootstrap_conf() -> String {
    let mut bootstrap = BTreeMap::new();
    // Java command to use when running NiFi
    bootstrap.insert("java", "java".to_string());
    // Username to use when running NiFi. This value will be ignored on Windows.
    bootstrap.insert("run.as", "".to_string());
    // Preserve shell environment while runnning as "run.as" user
    bootstrap.insert("preserve.environment", "false".to_string());
    // Configure where NiFi's lib and conf directories live
    bootstrap.insert("lib.dir", "./lib".to_string());
    bootstrap.insert("conf.dir", "./conf".to_string());
    // How long to wait after telling NiFi to shutdown before explicitly killing the Process
    bootstrap.insert("graceful.shutdown.seconds", "20".to_string());
    // Disable JSR 199 so that we can use JSP's without running a JDK
    bootstrap.insert(
        "java.arg.1",
        "-Dorg.apache.jasper.compiler.disablejsr199=true".to_string(),
    );
    // JVM memory settings
    bootstrap.insert("java.arg.2", "-Xms1024m".to_string());
    bootstrap.insert("java.arg.3", "-Xmx1024m".to_string());

    bootstrap.insert("java.arg.4", "-Djava.net.preferIPv4Stack=true".to_string());

    // allowRestrictedHeaders is required for Cluster/Node communications to work properly
    bootstrap.insert(
        "java.arg.5",
        "-Dsun.net.http.allowRestrictedHeaders=true".to_string(),
    );
    bootstrap.insert(
        "java.arg.6",
        "-Djava.protocol.handler.pkgs=sun.net.www.protocol".to_string(),
    );

    // The G1GC is known to cause some problems in Java 8 and earlier, but the issues were addressed in Java 9. If using Java 8 or earlier,
    // it is recommended that G1GC not be used, especially in conjunction with the Write Ahead Provenance Repository. However, if using a newer
    // version of Java, it can result in better performance without significant \"stop-the-world\" delays.
    //bootstrap.insert("java.arg.13", "-XX:+UseG1GC".to_string());

    // Set headless mode by default
    bootstrap.insert("java.arg.14", "-Djava.awt.headless=true".to_string());
    // Root key in hexadecimal format for encrypted sensitive configuration values
    //bootstrap.insert("nifi.bootstrap.sensitive.key=", "".to_string());
    // Sets the provider of SecureRandom to /dev/urandom to prevent blocking on VMs
    bootstrap.insert(
        "java.arg.15",
        "-Djava.security.egd=file:/dev/urandom".to_string(),
    );
    // Requires JAAS to use only the provided JAAS configuration to authenticate a Subject, without using any "fallback" methods (such as prompting for username/password)
    // Please see https://docs.oracle.com/javase/8/docs/technotes/guides/security/jgss/single-signon.html, section "EXCEPTIONS TO THE MODEL"
    bootstrap.insert(
        "java.arg.16",
        "-Djavax.security.auth.useSubjectCredsOnly=true".to_string(),
    );

    // Zookeeper 3.5 now includes an Admin Server that starts on port 8080, since NiFi is already using that port disable by default.
    // Please see https://zookeeper.apache.org/doc/current/zookeeperAdmin.html#sc_adminserver_config for configuration options.
    bootstrap.insert(
        "java.arg.17",
        "-Dzookeeper.admin.enableServer=false".to_string(),
    );

    format_properties(bootstrap)
}

/// Create the NiFi nifi.properties
pub fn build_nifi_properties(spec: &NifiSpec, zk_connect_string: &str) -> String {
    let mut properties = BTreeMap::new();
    // Core Properties
    properties.insert(
        "nifi.flow.configuration.file",
        "./conf/flow.xml.gz".to_string(),
    );
    properties.insert(
        "nifi.flow.configuration.archive.enabled",
        "true".to_string(),
    );
    properties.insert(
        "nifi.flow.configuration.archive.dir",
        "./conf/archive/".to_string(),
    );
    properties.insert(
        "nifi.flow.configuration.archive.max.time",
        "30 days".to_string(),
    );
    properties.insert(
        "nifi.flow.configuration.archive.max.storage",
        "500 MB".to_string(),
    );
    properties.insert("nifi.flow.configuration.archive.max.count", "".to_string());
    properties.insert("nifi.flowcontroller.autoResumeState", "true".to_string());
    properties.insert(
        "nifi.flowcontroller.graceful.shutdown.period",
        "10 sec".to_string(),
    );
    properties.insert("nifi.flowservice.writedelay.interval", "500 ms".to_string());
    properties.insert("nifi.administrative.yield.duration", "30 sec".to_string());
    // If a component has no work to do (is "bored"), how long should we wait before checking again for work?
    properties.insert("nifi.bored.yield.duration", "10 millis".to_string());
    properties.insert("nifi.queue.backpressure.count", "10000".to_string());
    properties.insert("nifi.queue.backpressure.size", "1 GB".to_string());

    properties.insert(
        "nifi.authorizer.configuration.file",
        "./conf/authorizers.xml".to_string(),
    );
    properties.insert(
        "nifi.login.identity.provider.configuration.file",
        "./conf/login-identity-providers.xml".to_string(),
    );
    properties.insert("nifi.templates.directory", "./conf/templates".to_string());
    properties.insert("nifi.ui.banner.text", "".to_string());
    properties.insert("nifi.ui.autorefresh.interval", "30 sec".to_string());
    properties.insert("nifi.nar.library.directory", "./lib".to_string());
    properties.insert(
        "nifi.nar.library.autoload.directory",
        "./extensions".to_string(),
    );
    properties.insert("nifi.nar.working.directory", "./work/nar/".to_string());
    properties.insert(
        "nifi.documentation.working.directory",
        "./work/docs/components".to_string(),
    );

    //###################
    // State Management #
    //###################
    properties.insert(
        "nifi.state.management.configuration.file",
        "./conf/state-management.xml".to_string(),
    );
    // The ID of the local state provider
    properties.insert(
        "nifi.state.management.provider.local",
        "local-provider".to_string(),
    );
    // The ID of the cluster-wide state provider. This will be ignored if NiFi is not clustered but must be populated if running in a cluster.
    properties.insert(
        "nifi.state.management.provider.cluster",
        "zk-provider".to_string(),
    );
    // Specifies whether or not this instance of NiFi should run an embedded ZooKeeper server
    properties.insert(
        "nifi.state.management.embedded.zookeeper.start",
        "false".to_string(),
    );

    // H2 Settings
    properties.insert(
        "nifi.database.directory",
        NifiRepository::Database.mount_path(),
    );
    properties.insert(
        "nifi.h2.url.append",
        ";LOCK_TIMEOUT=25000;WRITE_DELAY=0;AUTO_SERVER=FALSE".to_string(),
    );

    // Repository Encryption properties override individual repository implementation properties
    properties.insert(
        "nifi.repository.encryption.protocol.version",
        "".to_string(),
    );
    properties.insert("nifi.repository.encryption.key.id", "".to_string());
    properties.insert("nifi.repository.encryption.key.provider", "".to_string());
    properties.insert(
        "nifi.repository.encryption.key.provider.keystore.location",
        "".to_string(),
    );
    properties.insert(
        "nifi.repository.encryption.key.provider.keystore.password",
        "".to_string(),
    );

    // FlowFile Repository
    properties.insert(
        "nifi.flowfile.repository.implementation",
        "org.apache.nifi.controller.repository.WriteAheadFlowFileRepository".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.wal.implementation",
        "org.apache.nifi.wali.SequentialAccessWriteAheadLog".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.directory",
        NifiRepository::Flowfile.mount_path(),
    );
    properties.insert(
        "nifi.flowfile.repository.checkpoint.interval",
        "20 secs".to_string(),
    );
    properties.insert("nifi.flowfile.repository.always.sync", "false".to_string());
    properties.insert(
        "nifi.flowfile.repository.retain.orphaned.flowfiles",
        "true".to_string(),
    );

    properties.insert(
        "nifi.swap.manager.implementation",
        "org.apache.nifi.controller.FileSystemSwapManager".to_string(),
    );
    properties.insert("nifi.queue.swap.threshold", "20000".to_string());

    // Content Repository
    properties.insert(
        "nifi.content.repository.implementation",
        "org.apache.nifi.controller.repository.FileSystemRepository".to_string(),
    );
    properties.insert("nifi.content.claim.max.appendable.size", "1 MB".to_string());
    properties.insert(
        "nifi.content.repository.directory.default",
        NifiRepository::Content.mount_path(),
    );
    properties.insert(
        "nifi.content.repository.archive.max.retention.period",
        "7 days".to_string(),
    );
    properties.insert(
        "nifi.content.repository.archive.max.usage.percentage",
        "50%".to_string(),
    );
    properties.insert(
        "nifi.content.repository.archive.enabled",
        "true".to_string(),
    );
    properties.insert("nifi.content.repository.always.sync", "false".to_string());
    properties.insert(
        "nifi.content.viewer.url",
        "../nifi-content-viewer/".to_string(),
    );

    // Provenance Repository Properties
    properties.insert(
        "nifi.provenance.repository.implementation",
        "org.apache.nifi.provenance.WriteAheadProvenanceRepository".to_string(),
    );

    // Persistent Provenance Repository Properties
    properties.insert(
        "nifi.provenance.repository.directory.default",
        NifiRepository::Provenance.mount_path(),
    );
    properties.insert(
        "nifi.provenance.repository.max.storage.time",
        "30 days".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.max.storage.size",
        "10 GB".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.rollover.time",
        "10 mins".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.rollover.size",
        "100 MB".to_string(),
    );
    properties.insert("nifi.provenance.repository.query.threads", "2".to_string());
    properties.insert("nifi.provenance.repository.index.threads", "2".to_string());
    properties.insert(
        "nifi.provenance.repository.compress.on.rollover",
        "true".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.always.sync",
        "false".to_string(),
    );
    // Comma-separated list of fields. Fields that are not indexed will not be searchable. Valid fields are:
    // EventType, FlowFileUUID, Filename, TransitURI, ProcessorID, AlternateIdentifierURI, Relationship, Details
    properties.insert(
        "nifi.provenance.repository.indexed.fields",
        "EventType, FlowFileUUID, Filename, ProcessorID, Relationship".to_string(),
    );
    // FlowFile Attributes that should be indexed and made searchable.  Some examples to consider are filename, uuid, mime.type
    properties.insert(
        "nifi.provenance.repository.indexed.attributes",
        "".to_string(),
    );
    // Large values for the shard size will result in more Java heap usage when searching the Provenance Repository
    // but should provide better performance
    properties.insert(
        "nifi.provenance.repository.index.shard.size",
        "500 MB".to_string(),
    );
    // Indicates the maximum length that a FlowFile attribute can be when retrieving a Provenance Event from
    // the repository. If the length of any attribute exceeds this value, it will be truncated when the event is retrieved.
    properties.insert(
        "nifi.provenance.repository.max.attribute.length",
        "65536".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.concurrent.merge.threads",
        "2".to_string(),
    );

    // Volatile Provenance Respository Properties
    properties.insert(
        "nifi.provenance.repository.buffer.size",
        "100000".to_string(),
    );

    // Component Status Repository
    properties.insert(
        "nifi.components.status.repository.implementation",
        "org.apache.nifi.controller.status.history.VolatileComponentStatusRepository".to_string(),
    );
    properties.insert(
        "nifi.components.status.repository.buffer.size",
        "1440".to_string(),
    );
    properties.insert(
        "nifi.components.status.snapshot.frequency",
        "1 min".to_string(),
    );

    // QuestDB Status History Repository Properties
    properties.insert(
        "nifi.status.repository.questdb.persist.node.days",
        "14".to_string(),
    );
    properties.insert(
        "nifi.status.repository.questdb.persist.component.days",
        "3".to_string(),
    );
    properties.insert(
        "nifi.status.repository.questdb.persist.location",
        "./status_repository".to_string(),
    );

    //#############################################

    properties.insert("nifi.web.https.host", "0.0.0.0".to_string());
    properties.insert("nifi.web.https.port", HTTPS_PORT.to_string());
    properties.insert("nifi.web.https.network.interface.default", "".to_string());
    properties.insert(
        "nifi.web.jetty.working.directory",
        "./work/jetty".to_string(),
    );
    properties.insert("nifi.web.jetty.threads", "200".to_string());
    properties.insert("nifi.web.max.header.size", "16 KB".to_string());
    properties.insert("nifi.web.proxy.context.path", "".to_string());
    properties.insert("nifi.web.proxy.host", "".to_string());
    properties.insert("nifi.web.max.content.size", "".to_string());
    properties.insert("nifi.web.max.requests.per.second", "30000".to_string());
    properties.insert(
        "nifi.web.max.access.token.requests.per.second",
        "25".to_string(),
    );
    properties.insert("nifi.web.request.timeout", "60 secs".to_string());
    properties.insert("nifi.web.request.ip.whitelist", "".to_string());
    properties.insert("nifi.web.should.send.server.version", "true".to_string());

    // Include or Exclude TLS Cipher Suites for HTTPS
    properties.insert("nifi.web.https.ciphersuites.include", "".to_string());
    properties.insert("nifi.web.https.ciphersuites.exclude", "".to_string());

    // security properties
    // this property is later set from a secret
    properties.insert("nifi.sensitive.props.key", "".to_string());
    properties.insert("nifi.sensitive.props.key.protected", "".to_string());
    properties.insert(
        "nifi.sensitive.props.algorithm",
        "NIFI_PBKDF2_AES_GCM_256".to_string(),
    );
    properties.insert("nifi.sensitive.props.additional.keys", "".to_string());

    properties.insert("nifi.security.autoreload.enabled", "false".to_string());
    properties.insert("nifi.security.autoreload.interval", "10 secs".to_string());
    // ke
    properties.insert(
        "nifi.security.keystore",
        "/stackable/keystore/keystore.jks".to_string(),
    );
    properties.insert("nifi.security.keystoreType", "JKS".to_string());
    properties.insert("nifi.security.keystorePasswd", "secret".to_string());
    properties.insert(
        "nifi.security.truststore",
        "/stackable/keystore/truststore.jks".to_string(),
    );
    properties.insert("nifi.security.truststoreType", "JKS".to_string());
    properties.insert("nifi.security.truststorePasswd", "secret".to_string());

    properties.insert(
        "nifi.security.user.login.identity.provider",
        "single-user-provider".to_string(),
    );

    properties.insert(
        "nifi.security.user.authorizer",
        "managed-authorizer".to_string(),
    );
    properties.insert(
        "nifi.security.allow.anonymous.authentication",
        "false".to_string(),
    );
    //properties.insert("nifi.security.user.login.identity.provider", "".to_string());
    properties.insert(
        "nifi.security.user.jws.key.rotation.period",
        "PT1H".to_string(),
    );
    properties.insert("nifi.security.ocsp.responder.url", "".to_string());
    properties.insert("nifi.security.ocsp.responder.certificate", "".to_string());

    // OpenId Connect SSO Properties
    properties.insert("nifi.security.user.oidc.discovery.url", "".to_string());
    properties.insert(
        "nifi.security.user.oidc.connect.timeout",
        "5 secs".to_string(),
    );
    properties.insert("nifi.security.user.oidc.read.timeout", "5 secs".to_string());
    properties.insert("nifi.security.user.oidc.client.id", "".to_string());
    properties.insert("nifi.security.user.oidc.client.secret", "".to_string());
    properties.insert(
        "nifi.security.user.oidc.preferred.jwsalgorithm",
        "".to_string(),
    );
    properties.insert("nifi.security.user.oidc.additional.scopes", "".to_string());
    properties.insert(
        "nifi.security.user.oidc.claim.identifying.user",
        "".to_string(),
    );
    properties.insert(
        "nifi.security.user.oidc.fallback.claims.identifying.user",
        "".to_string(),
    );

    // Apache Knox SSO Properties
    properties.insert("nifi.security.user.knox.url", "".to_string());
    properties.insert("nifi.security.user.knox.publicKey", "".to_string());
    properties.insert(
        "nifi.security.user.knox.cookieName",
        "hadoop-jwt".to_string(),
    );
    properties.insert("nifi.security.user.knox.audiences", "".to_string());

    // SAML Properties
    properties.insert("nifi.security.user.saml.idp.metadata.url", "".to_string());
    properties.insert("nifi.security.user.saml.sp.entity.id", "".to_string());
    properties.insert(
        "nifi.security.user.saml.identity.attribute.name",
        "".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.group.attribute.name",
        "".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.metadata.signing.enabled",
        "false".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.request.signing.enabled",
        "false".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.want.assertions.signed",
        "true".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.signature.algorithm",
        "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.signature.digest.algorithm",
        "http://www.w3.org/2001/04/xmlenc#sha256".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.message.logging.enabled",
        "false".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.authentication.expiration",
        "12 hours".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.single.logout.enabled",
        "false".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.http.client.truststore.strategy",
        "JDK".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.http.client.connect.timeout",
        "30 secs".to_string(),
    );
    properties.insert(
        "nifi.security.user.saml.http.client.read.timeout",
        "30 secs".to_string(),
    );

    // cluster common properties (all nodes must have same values)
    properties.insert(
        "nifi.cluster.protocol.heartbeat.interval",
        "5 sec".to_string(),
    );
    properties.insert(
        "nifi.cluster.protocol.heartbeat.missable.max",
        "8".to_string(),
    );
    properties.insert("nifi.cluster.protocol.is.secure", "true".to_string());

    // cluster node properties (only configure for cluster nodes)
    properties.insert("nifi.cluster.is.node", "true".to_string());

    // this will be overwritten to the correct FQDN in the container start command
    properties.insert("nifi.cluster.node.address", "".to_string());

    properties.insert("nifi.cluster.node.protocol.port", PROTOCOL_PORT.to_string());
    properties.insert("nifi.cluster.node.protocol.threads", "10".to_string());
    properties.insert("nifi.cluster.node.protocol.max.threads", "50".to_string());
    properties.insert("nifi.cluster.node.event.history.size", "25".to_string());
    properties.insert("nifi.cluster.node.connection.timeout", "5 sec".to_string());
    properties.insert("nifi.cluster.node.read.timeout", "5 sec".to_string());
    properties.insert(
        "nifi.cluster.node.max.concurrent.requests",
        "100".to_string(),
    );
    properties.insert("nifi.cluster.firewall.file", "".to_string());
    // TODO: set to 1 min for testing (default 5)
    properties.insert(
        "nifi.cluster.flow.election.max.wait.time",
        "1 mins".to_string(),
    );
    properties.insert("nifi.cluster.flow.election.max.candidates", "".to_string());

    // cluster load balancing properties
    properties.insert("nifi.cluster.load.balance.host", "".to_string());

    properties.insert(
        "nifi.cluster.load.balance.connections.per.node",
        "1".to_string(),
    );
    properties.insert(
        "nifi.cluster.load.balance.max.thread.count",
        "8".to_string(),
    );
    properties.insert(
        "nifi.cluster.load.balance.comms.timeout",
        "30 sec".to_string(),
    );

    // zookeeper properties, used for cluster management
    properties.insert(
        "nifi.zookeeper.connect.string",
        zk_connect_string.to_string(),
    );
    properties.insert("nifi.zookeeper.connect.timeout", "10 secs".to_string());
    properties.insert("nifi.zookeeper.session.timeout", "10 secs".to_string());
    properties.insert(
        "nifi.zookeeper.root.node",
        spec.zookeeper_reference
            .chroot
            .as_deref()
            .unwrap_or("nifi")
            .to_string(),
    );
    properties.insert("nifi.zookeeper.client.secure", "false".to_string());
    properties.insert("nifi.zookeeper.security.keystore", "".to_string());
    properties.insert("nifi.zookeeper.security.keystoreType", "".to_string());
    properties.insert("nifi.zookeeper.security.keystorePasswd", "".to_string());
    properties.insert("nifi.zookeeper.security.truststore", "".to_string());
    properties.insert("nifi.zookeeper.security.truststoreType", "".to_string());
    properties.insert("nifi.zookeeper.security.truststorePasswd", "".to_string());
    properties.insert("nifi.zookeeper.jute.maxbuffer", "".to_string());

    // Zookeeper properties for the authentication scheme used when creating acls on znodes used for cluster management
    // Values supported for nifi.zookeeper.auth.type are "default", which will apply world/anyone rights on znodes
    // and "sasl" which will give rights to the sasl/kerberos identity used to authenticate the nifi node
    // The identity is determined using the value in nifi.kerberos.service.principal and the removeHostFromPrincipal
    // and removeRealmFromPrincipal values (which should align with the kerberos.removeHostFromPrincipal and kerberos.removeRealmFromPrincipal
    // values configured on the zookeeper server).
    properties.insert("nifi.zookeeper.auth.type", "".to_string());
    properties.insert(
        "nifi.zookeeper.kerberos.removeHostFromPrincipal",
        "".to_string(),
    );
    properties.insert(
        "nifi.zookeeper.kerberos.removeRealmFromPrincipal",
        "".to_string(),
    );

    // kerberos
    properties.insert("nifi.kerberos.krb5.file", "".to_string());

    // kerberos service principal
    properties.insert("nifi.kerberos.service.principal", "".to_string());
    properties.insert("nifi.kerberos.service.keytab.location", "".to_string());

    // kerberos spnego principal
    properties.insert("nifi.kerberos.spnego.principal", "".to_string());
    properties.insert("nifi.kerberos.spnego.keytab.location", "".to_string());
    properties.insert(
        "nifi.kerberos.spnego.authentication.expiration",
        "12 hours".to_string(),
    );

    // external properties files for variable registry
    // supports a comma delimited list of file locations
    properties.insert("nifi.variable.registry.properties", "".to_string());

    // analytics properties
    properties.insert("nifi.analytics.predict.enabled", "false".to_string());
    properties.insert("nifi.analytics.predict.interval", "3 mins".to_string());
    properties.insert("nifi.analytics.query.interval", "5 mins".to_string());
    properties.insert(
        "nifi.analytics.connection.model.implementation",
        "org.apache.nifi.controller.status.analytics.models.OrdinaryLeastSquares".to_string(),
    );
    properties.insert(
        "nifi.analytics.connection.model.score.name",
        "rSquared".to_string(),
    );
    properties.insert(
        "nifi.analytics.connection.model.score.threshold",
        ".90".to_string(),
    );

    // runtime monitoring properties
    properties.insert("nifi.monitor.long.running.task.schedule", "".to_string());
    properties.insert("nifi.monitor.long.running.task.threshold", "".to_string());

    // Create automatic diagnostics when stopping/restarting NiFi.

    // Enable automatic diagnostic at shutdown.
    properties.insert("nifi.diagnostics.on.shutdown.enabled", "false".to_string());

    // Include verbose diagnostic information.
    properties.insert("nifi.diagnostics.on.shutdown.verbose", "false".to_string());

    // The location of the diagnostics folder.
    properties.insert(
        "nifi.diagnostics.on.shutdown.directory",
        "./diagnostics".to_string(),
    );

    // The maximum number of files permitted in the directory. If the limit is exceeded, the oldest files are deleted.
    properties.insert(
        "nifi.diagnostics.on.shutdown.max.filecount",
        "10".to_string(),
    );

    // The diagnostics folder's maximum permitted size in bytes. If the limit is exceeded, the oldest files are deleted.
    properties.insert(
        "nifi.diagnostics.on.shutdown.max.directory.size",
        "10 MB".to_string(),
    );

    format_properties(properties)
}

pub fn build_state_management_xml(spec: &NifiSpec, zk_connect_string: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>
        <stateManagement>
          <local-provider>
          <id>local-provider</id>
            <class>org.apache.nifi.controller.state.providers.local.WriteAheadLocalStateProvider</class>
            <property name=\"Directory\">./state/local</property>
            <property name=\"Always Sync\">false</property>
            <property name=\"Partitions\">16</property>
            <property name=\"Checkpoint Interval\">2 mins</property>
          </local-provider>
          <cluster-provider>
            <id>zk-provider</id>
            <class>org.apache.nifi.controller.state.providers.zookeeper.ZooKeeperStateProvider</class>
            <property name=\"Connect String\">{}</property>
            <property name=\"Root Node\">{}</property>
            <property name=\"Session Timeout\">10 seconds</property>
            <property name=\"Access Control\">Open</property>
          </cluster-provider>
        </stateManagement>",
        zk_connect_string,
        &spec
            .zookeeper_reference
            .chroot.as_deref()
            .unwrap_or("")
    )
}

/// Defines all required roles and their required configuration. In this case we need three files:
/// `bootstrap.conf`, `nifi.properties` and `state-management.xml`.
///
/// We do not require any env variables yet. We will however utilize them to change the
/// configuration directory (check https://github.com/apache/nifi/pull/2985).
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
    role: &Role<NifiConfig>,
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
                PropertyNameKind::Env,
            ],
            role.clone(),
        ),
    );

    let role_config =
        transform_all_roles_to_config(resource, roles).context(ProductConfigTransform)?;

    validate_all_roles_and_groups_config(version, &role_config, product_config, false, false)
        .context(InvalidProductConfig)
}

// TODO: Use crate like https://crates.io/crates/java-properties (currently does not work for Nifi
//    because of escapes), to have save handling of escapes etc.
fn format_properties(properties: BTreeMap<&str, String>) -> String {
    let mut result = String::new();

    for (key, value) in properties {
        result.push_str(&format!("{}={}\n", key, value));
    }

    result
}
