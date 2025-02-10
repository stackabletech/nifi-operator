use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    memory::{BinaryMultiple, MemoryQuantity},
    role_utils::{self, GenericRoleConfig, JavaCommonConfig, JvmArgumentOverrides, Role},
};

use crate::{
    config::{JVM_SECURITY_PROPERTIES_FILE, NIFI_CONFIG_DIRECTORY},
    crd::{NifiConfig, NifiConfigFragment},
};

// Part of memory resources allocated for Java heap
const JAVA_HEAP_FACTOR: f32 = 0.8;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("invalid memory resource configuration - missing default or value in crd?"))]
    MissingMemoryResourceConfig,

    #[snafu(display("invalid memory config"))]
    InvalidMemoryConfig {
        source: stackable_operator::memory::Error,
    },

    #[snafu(display("failed to merge jvm argument overrides"))]
    MergeJvmArgumentOverrides { source: role_utils::Error },
}

/// Create the NiFi bootstrap.conf
pub fn build_merged_jvm_config(
    merged_config: &NifiConfig,
    role: &Role<NifiConfigFragment, GenericRoleConfig, JavaCommonConfig>,
    role_group: &str,
) -> Result<JvmArgumentOverrides, Error> {
    let heap_size = MemoryQuantity::try_from(
        merged_config
            .resources
            .memory
            .limit
            .as_ref()
            .context(MissingMemoryResourceConfigSnafu)?,
    )
    .context(InvalidMemoryConfigSnafu)?
    .scale_to(BinaryMultiple::Mebi)
        * JAVA_HEAP_FACTOR;
    let java_heap = heap_size
        .format_for_java()
        .context(InvalidMemoryConfigSnafu)?;

    let jvm_args = vec![
        // Heap settings
        format!("-Xmx{java_heap}"),
        format!("-Xms{java_heap}"),
        // The G1GC is known to cause some problems in Java 8 and earlier, but the issues were addressed in Java 9. If using Java 8 or earlier,
        // it is recommended that G1GC not be used, especially in conjunction with the Write Ahead Provenance Repository. However, if using a newer
        // version of Java, it can result in better performance without significant \"stop-the-world\" delays.
        "-XX:+UseG1GC".to_owned(),
        // Set headless mode by default
        "-Djava.awt.headless=true".to_owned(),
        // Disable JSR 199 so that we can use JSP's without running a JDK
        "-Dorg.apache.jasper.compiler.disablejsr199=true".to_owned(),
        // Note(sbernauer): This has been here since ages, leaving it here for compatibility reasons.
        // That being said: IPV6 rocks :rocket:!
        "-Djava.net.preferIPv4Stack=true".to_owned(),
        // allowRestrictedHeaders is required for Cluster/Node communications to work properly
        "-Dsun.net.http.allowRestrictedHeaders=true".to_owned(),
        "-Djava.protocol.handler.pkgs=sun.net.www.protocol".to_owned(),
        // Sets the provider of SecureRandom to /dev/urandom to prevent blocking on VMs
        "-Djava.security.egd=file:/dev/urandom".to_owned(),
        // Requires JAAS to use only the provided JAAS configuration to authenticate a Subject, without using any "fallback" methods (such as prompting for username/password)
        // Please see https://docs.oracle.com/javase/8/docs/technotes/guides/security/jgss/single-signon.html, section "EXCEPTIONS TO THE MODEL"
        "-Djavax.security.auth.useSubjectCredsOnly=true".to_owned(),
        // Zookeeper 3.5 now includes an Admin Server that starts on port 8080, since NiFi is already using that port disable by default.
        // Please see https://zookeeper.apache.org/doc/current/zookeeperAdmin.html#sc_adminserver_config for configuration options.
        "-Dzookeeper.admin.enableServer=false".to_owned(),
        // JVM security properties include especially TTL values for the positive and negative DNS caches.
        format!(
            "-Djava.security.properties={NIFI_CONFIG_DIRECTORY}/{JVM_SECURITY_PROPERTIES_FILE}"
        ),
    ];

    let operator_generated = JvmArgumentOverrides::new_with_only_additions(jvm_args);
    role.get_merged_jvm_argument_overrides(role_group, &operator_generated)
        .context(MergeJvmArgumentOverridesSnafu)
}
