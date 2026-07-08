//! Builder for `bootstrap.conf`.

use std::collections::BTreeMap;

use snafu::{ResultExt, Snafu};

use crate::{
    controller::{
        NifiRoleGroupConfig,
        build::{
            graceful_shutdown::graceful_shutdown_config_properties, jvm::build_merged_jvm_config,
        },
    },
    security::authorization::ResolvedNifiAuthorizationConfig,
};

/// Errors that can occur while building `bootstrap.conf`.
#[derive(Snafu, Debug)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("invalid JVM config"))]
    InvalidJVMConfig {
        source: crate::controller::build::jvm::Error,
    },
}

pub fn build(
    rg: &NifiRoleGroupConfig,
    authorization_config: Option<&ResolvedNifiAuthorizationConfig>,
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
    bootstrap.extend(graceful_shutdown_config_properties(&rg.config));

    let jvm_args = build_merged_jvm_config(
        &rg.config,
        &rg.product_specific_common_config.jvm_argument_overrides,
        authorization_config,
    )
    .context(InvalidJVMConfigSnafu)?;

    for (index, argument) in jvm_args.iter().enumerate() {
        bootstrap.insert(format!("java.arg.{}", index + 1), argument.clone());
    }

    // configOverrides come last
    bootstrap.extend(rg.config_overrides.bootstrap_conf.overrides.clone());

    Ok(super::format_properties(bootstrap))
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use stackable_operator::v2::types::operator::RoleGroupName;

    use super::*;
    use crate::{
        controller::validate::{build_role_group_configs, test_resolved_product_image},
        crd::{NifiRole, v1alpha1},
    };

    fn construct_bootstrap_conf(nifi_cluster: &str) -> String {
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::from_str(nifi_cluster).expect("illegal test input");

        let role_group_configs =
            build_role_group_configs(&nifi, &test_resolved_product_image(), &None)
                .expect("failed to build role group configs");
        let rg = role_group_configs
            .get(&NifiRole::Node)
            .and_then(|groups| {
                groups.get(
                    &"default"
                        .parse::<RoleGroupName>()
                        .expect("valid role-group name"),
                )
            })
            .expect("default role group must exist");

        build(rg, None).unwrap()
    }

    #[test]
    fn test_build_bootstrap_conf_defaults() {
        let input = r#"
        apiVersion: nifi.stackable.tech/v1alpha1
        kind: NifiCluster
        metadata:
          name: simple-nifi
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
        "#;
        let bootstrap_conf = construct_bootstrap_conf(input);

        assert_eq!(
            bootstrap_conf,
            indoc! {"
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
            "}
        );
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
            productVersion: 2.9.0
          clusterConfig:
            authentication:
              - authenticationClass: nifi-admin-credentials-simple
            sensitiveProperties:
              keySecret: simple-nifi-sensitive-property-key
              autoGenerate: true
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

        assert_eq!(
            bootstrap_conf,
            indoc! {"
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
            "}
        );
    }
}
