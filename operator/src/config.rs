use stackable_nifi_crd::{NiFiConfig, NiFiSpec};
use std::collections::BTreeMap;

pub fn create_bootstrap_conf() -> String {
    let mut bootstrap = BTreeMap::new();
    // Java command to use when running NiFi
    bootstrap.insert("java", "java".to_string());
    // Username to use when running NiFi. This value will be ignored on Windows.
    bootstrap.insert("run.as", "".to_string());
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
    // TODO: adapt to config
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

    // The following options configure a Java Agent to handle native library loading.
    // It is needed when a custom jar (eg. JDBC driver) has been configured on a component in the flow and this custom jar depends on a native library
    // and tries to load it by its absolute path (java.lang.System.load(String filename) method call).
    // Use this Java Agent only if you get "Native Library ... already loaded in another classloader" errors otherwise!
    //bootstrap.insert(
    //    "java.arg.18",
    //    "-javaagent:./lib/aspectj/aspectjweaver-1.9.6.jar".to_string(),
    //);
    //bootstrap.insert("java.arg.19", "-Daj.weaving.loadersToSkip=sun.misc.Launcher$AppClassLoader,jdk.internal.loader.ClassLoaders$AppClassLoader,org.eclipse.jetty.webapp.WebAppClassLoader,org.apache.jasper.servlet.JasperLoader,org.jvnet.hk2.internal.DelegatingClassLoader,org.apache.nifi.nar.NarClassLoader".to_string());

    // XML File that contains the definitions of the notification services
    // TODO: adapt "conf" to {{configroot}}
    //bootstrap.insert(
    //    "notification.services.file",
    //    "./conf/bootstrap-notification-services.xml".to_string(),
    //);

    // In the case that we are unable to send a notification for an event, how many times should we retry?
    //bootstrap.insert("notification.max.attempts", "5".to_string());
    // Comma-separated list of identifiers that are present in the notification.services.file; which services should be used to notify when NiFi is started?
    //bootstrap.insert("nifi.start.notification.services", "email-notification".to_string());
    // Comma-separated list of identifiers that are present in the notification.services.file; which services should be used to notify when NiFi is stopped?
    //bootstrap.insert("nifi.stop.notification.services", "email-notification".to_string());

    format_properties(bootstrap)
}

pub fn create_nifi_properties(spec: &NiFiSpec, config: &NiFiConfig, zk_ref: &str) -> String {
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
    // Properties file that provides the ZooKeeper properties to use if <nifi.state.management.embedded.zookeeper.start> is set to true
    //properties.insert(
    //    "nifi.state.management.embedded.zookeeper.properties",
    //    "./conf/zookeeper.properties".to_string(),
    //);

    // H2 Settings
    properties.insert(
        "nifi.database.directory",
        "./database_repository".to_string(),
    );
    properties.insert(
        "nifi.h2.url.append",
        ";LOCK_TIMEOUT=25000;WRITE_DELAY=0;AUTO_SERVER=FALSE".to_string(),
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
        "./flowfile_repository".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.checkpoint.interval",
        "20 secs".to_string(),
    );
    properties.insert("nifi.flowfile.repository.always.sync", "false".to_string());
    properties.insert(
        "nifi.flowfile.repository.encryption.key.provider.implementation",
        "".to_string(),
    );
    properties.insert(
        "nifi.flowfile.repository.encryption.key.provider.location",
        "".to_string(),
    );
    properties.insert("nifi.flowfile.repository.encryption.key.id", "".to_string());
    properties.insert("nifi.flowfile.repository.encryption.key", "".to_string());
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
        "./content_repository".to_string(),
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
    properties.insert(
        "nifi.content.repository.encryption.key.provider.implementation",
        "".to_string(),
    );
    properties.insert(
        "nifi.content.repository.encryption.key.provider.location",
        "".to_string(),
    );
    properties.insert("nifi.content.repository.encryption.key.id", "".to_string());
    properties.insert("nifi.content.repository.encryption.key", "".to_string());

    // Provenance Repository Properties
    properties.insert(
        "nifi.provenance.repository.implementation",
        "org.apache.nifi.provenance.WriteAheadProvenanceRepository".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.encryption.key.provider.implementation",
        "".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.encryption.key.provider.location",
        "".to_string(),
    );
    properties.insert(
        "nifi.provenance.repository.encryption.key.id",
        "".to_string(),
    );
    properties.insert("nifi.provenance.repository.encryption.key", "".to_string());

    // Persistent Provenance Repository Properties
    properties.insert(
        "nifi.provenance.repository.directory.default",
        "./provenance_repository".to_string(),
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

    // Site to Site properties
    properties.insert("nifi.remote.input.host", "".to_string());
    properties.insert("nifi.remote.input.secure", "false".to_string());
    properties.insert("nifi.remote.input.socket.port", "".to_string());
    properties.insert("nifi.remote.input.http.enabled", "true".to_string());
    properties.insert(
        "nifi.remote.input.http.transaction.ttl",
        "30 sec".to_string(),
    );
    properties.insert(
        "nifi.remote.contents.cache.expiration",
        "30 secs".to_string(),
    );

    //#################
    // web properties #
    //#################
    // For security, NiFi will present the UI on 127.0.0.1 and only be accessible through this loopback interface.
    // Be aware that changing these properties may affect how your instance can be accessed without any restriction.
    // We recommend configuring HTTPS instead. The administrators guide provides instructions on how to do this.

    properties.insert("nifi.web.http.host", "127.0.0.1".to_string());

    if let Some(http_port) = &config.http_port {
        properties.insert("nifi.web.http.port", http_port.to_string());
    }

    properties.insert("nifi.web.http.network.interface.default", "".to_string());

    //#############################################

    properties.insert("nifi.web.https.host", "".to_string());
    properties.insert("nifi.web.https.port", "".to_string());
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
    properties.insert("nifi.web.should.send.server.version", "true".to_string());

    // security properties
    properties.insert("nifi.sensitive.props.key", "".to_string());
    properties.insert("nifi.sensitive.props.key.protected", "".to_string());
    properties.insert(
        "nifi.sensitive.props.algorithm",
        "PBEWITHMD5AND256BITAES-CBC-OPENSSL".to_string(),
    );
    properties.insert("nifi.sensitive.props.provider", "BC".to_string());
    properties.insert("nifi.sensitive.props.additional.keys", "".to_string());

    properties.insert("nifi.security.keystore", "".to_string());
    properties.insert("nifi.security.keystoreType", "".to_string());
    properties.insert("nifi.security.keystorePasswd", "".to_string());
    properties.insert("nifi.security.keyPasswd", "".to_string());
    properties.insert("nifi.security.truststore", "".to_string());
    properties.insert("nifi.security.truststoreType", "".to_string());
    properties.insert("nifi.security.truststorePasswd", "".to_string());
    properties.insert(
        "nifi.security.user.authorizer",
        "managed-authorizer".to_string(),
    );
    properties.insert(
        "nifi.security.allow.anonymous.authentication",
        "false".to_string(),
    );
    properties.insert("nifi.security.user.login.identity.provider", "".to_string());
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

    // Identity Mapping Properties
    // These properties allow normalizing user identities such that identities coming from different identity providers
    // (certificates, LDAP, Kerberos) can be treated the same internally in NiFi. The following example demonstrates normalizing
    // DNs from certificates and principals from Kerberos into a common identity string:
    // properties.insert("nifi.security.identity.mapping.pattern.dn", "^CN=(.*?), OU=(.*?), O=(.*?), L=(.*?), ST=(.*?), C=(.*?)$".to_string());
    // properties.insert("nifi.security.identity.mapping.value.dn", "$1@$2".to_string());
    // properties.insert("nifi.security.identity.mapping.transform.dn", "NONE".to_string());
    // properties.insert("nifi.security.identity.mapping.pattern.kerb", "^(.*?)/instance@(.*?)$".to_string());
    // properties.insert("nifi.security.identity.mapping.value.kerb", "$1@$2".to_string());
    // properties.insert("nifi.security.identity.mapping.transform.kerb", "UPPER".to_string());

    // Group Mapping Properties
    // These properties allow normalizing group names coming from external sources like LDAP. The following example
    // lowercases any group name.
    // properties.insert("nifi.security.group.mapping.pattern.anygroup", "^(.*)$".to_string());
    // properties.insert("nifi.security.group.mapping.value.anygroup", "$1".to_string());
    // properties.insert("nifi.security.group.mapping.transform.anygroup", "LOWER".to_string());

    // cluster common properties (all nodes must have same values)
    properties.insert(
        "nifi.cluster.protocol.heartbeat.interval",
        "5 sec".to_string(),
    );
    properties.insert(
        "nifi.cluster.protocol.heartbeat.missable.max",
        "8".to_string(),
    );
    properties.insert("nifi.cluster.protocol.is.secure", "false".to_string());

    // cluster node properties (only configure for cluster nodes)
    properties.insert("nifi.cluster.is.node", "true".to_string());
    properties.insert("nifi.cluster.node.address", "mdesktop".to_string());
    if let Some(node_protocol_port) = &config.node_protocol_port {
        properties.insert(
            "nifi.cluster.node.protocol.port",
            node_protocol_port.to_string(),
        );
    }
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
    properties.insert(
        "nifi.cluster.flow.election.max.wait.time",
        "5 mins".to_string(),
    );
    properties.insert("nifi.cluster.flow.election.max.candidates", "".to_string());

    // cluster load balancing properties
    properties.insert("nifi.cluster.load.balance.host", "".to_string());
    if let Some(node_load_balancing_port) = &config.node_load_balancing_port {
        properties.insert(
            "nifi.cluster.load.balance.port",
            node_load_balancing_port.to_string(),
        );
    }

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
    properties.insert("nifi.zookeeper.connect.string", zk_ref.to_string());
    properties.insert("nifi.zookeeper.connect.timeout", "10 secs".to_string());
    properties.insert("nifi.zookeeper.session.timeout", "10 secs".to_string());
    properties.insert(
        "nifi.zookeeper.root.node",
        spec.zookeeper_reference
            .chroot
            .clone()
            .unwrap_or_else(|| "".to_string()),
    );
    properties.insert("nifi.zookeeper.client.secure", "false".to_string());
    properties.insert("nifi.zookeeper.security.keystore", "".to_string());
    properties.insert("nifi.zookeeper.security.keystoreType", "".to_string());
    properties.insert("nifi.zookeeper.security.keystorePasswd", "".to_string());
    properties.insert("nifi.zookeeper.security.truststore", "".to_string());
    properties.insert("nifi.zookeeper.security.truststoreType", "".to_string());
    properties.insert("nifi.zookeeper.security.truststorePasswd", "".to_string());

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

    format_properties(properties)
}

pub fn create_state_management_xml(spec: &NiFiSpec, zk_ref: &str) -> String {
    let mut state_management = String::new();

    state_management.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    state_management.push_str("<stateManagement>\n");

    state_management.push_str("  <local-provider>\n");
    state_management.push_str("    <id>local-provider</id>\n");
    state_management.push_str("    <class>org.apache.nifi.controller.state.providers.local.WriteAheadLocalStateProvider</class>\n");
    state_management.push_str("    <property name=\"Directory\">./state/local</property>\n");
    state_management.push_str("    <property name=\"Always Sync\">false</property>\n");
    state_management.push_str("    <property name=\"Partitions\">16</property>\n");
    state_management.push_str("    <property name=\"Checkpoint Interval\">2 mins</property>\n");
    state_management.push_str("  </local-provider>\n");

    state_management.push_str("  <cluster-provider>\n");
    state_management.push_str("    <id>zk-provider</id>\n");
    state_management.push_str("    <class>org.apache.nifi.controller.state.providers.zookeeper.ZooKeeperStateProvider</class>\n");
    // TODO: adapt connect string
    state_management.push_str(&format!(
        "    <property name=\"Connect String\">{}</property>\n",
        zk_ref
    ));
    state_management.push_str(&format!(
        "    <property name=\"Root Node\">{}</property>\n",
        &spec
            .zookeeper_reference
            .chroot
            .clone()
            .unwrap_or_else(|| "".to_string())
    ));
    state_management.push_str("    <property name=\"Session Timeout\">10 seconds</property>\n");
    state_management.push_str("    <property name=\"Access Control\">Open</property>\n");
    state_management.push_str("  </cluster-provider>\n");

    state_management.push_str("</stateManagement>\n");
    state_management
}

fn format_properties(properties: BTreeMap<&str, String>) -> String {
    let mut result = String::new();

    for (key, value) in properties {
        result.push_str(&format!("{}={}\n", key, value));
    }

    result
}
