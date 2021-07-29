mod config;
mod error;

use crate::config::{
    build_bootstrap_conf, build_nifi_properties, build_state_management_xml,
    validated_product_config,
};
use crate::error::Error;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::{ConfigMap, EnvVar, Node, Pod};
use kube::api::ListParams;
use kube::error::ErrorResponse;
use kube::Api;
use kube::ResourceExt;
use product_config::types::PropertyNameKind;
use product_config::ProductConfigManager;
use stackable_nifi_crd::{
    NifiCluster, NifiRole, NifiSpec, APP_NAME, MANAGED_BY, NIFI_CLUSTER_LOAD_BALANCE_PORT,
    NIFI_CLUSTER_NODE_PROTOCOL_PORT, NIFI_WEB_HTTP_PORT,
};
use stackable_operator::builder::{
    ConfigMapBuilder, ContainerBuilder, ObjectMetaBuilder, PodBuilder,
};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::error::OperatorResult;
use stackable_operator::labels::{
    build_common_labels_for_all_managed_resources, get_recommended_labels, APP_COMPONENT_LABEL,
    APP_INSTANCE_LABEL, APP_MANAGED_BY_LABEL, APP_NAME_LABEL, APP_VERSION_LABEL,
};
use stackable_operator::product_config_utils::{
    config_for_role_and_group, ValidatedRoleConfigByPropertyKind,
};
use stackable_operator::reconcile::{
    ContinuationStrategy, ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use stackable_operator::role_utils::{
    get_role_and_group_labels, list_eligible_nodes_for_role_and_group,
};
use stackable_operator::{k8s_utils, pod_utils, role_utils};
use stackable_zookeeper_crd::util::ZookeeperConnectionInformation;
use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use strum::IntoEnumIterator;
use tracing::{debug, info, trace, warn};

const FINALIZER_NAME: &str = "nifi.stackable.tech/cleanup";

type NifiReconcileResult = ReconcileResult<error::Error>;

struct NifiState {
    context: ReconciliationContext<NifiCluster>,
    existing_pods: Vec<Pod>,
    eligible_nodes: HashMap<String, HashMap<String, Vec<Node>>>,
    zookeeper_info: Option<ZookeeperConnectionInformation>,
    validated_role_config: ValidatedRoleConfigByPropertyKind,
}

impl NifiState {
    async fn get_zookeeper_connection_information(&mut self) -> NifiReconcileResult {
        let mut zk_ref: stackable_zookeeper_crd::util::ZookeeperReference =
            self.context.resource.spec.zookeeper_reference.clone();

        if let Some(chroot) = zk_ref.chroot.as_deref() {
            stackable_zookeeper_crd::util::is_valid_zookeeper_path(chroot)?;
        }

        // retrieve zookeeper connect string
        // we have to remove the chroot to only get the url and port
        // nifi has its own config properties for the chroot and fails if the
        // connect string is passed like: zookeeper_node:2181/nifi
        zk_ref.chroot = None;

        let zookeeper_info =
            stackable_zookeeper_crd::util::get_zk_connection_info(&self.context.client, &zk_ref)
                .await?;

        debug!(
            "Received ZooKeeper connect string: [{}]",
            &zookeeper_info.connection_string
        );

        self.zookeeper_info = Some(zookeeper_info);

        Ok(ReconcileFunctionAction::Continue)
    }

    /// Required labels for pods. Pods without any of these will be deleted and replaced.
    pub fn required_pod_labels(&self) -> BTreeMap<String, Option<Vec<String>>> {
        let roles = NifiRole::iter()
            .map(|role| role.to_string())
            .collect::<Vec<_>>();
        let mut mandatory_labels = BTreeMap::new();

        mandatory_labels.insert(String::from(APP_COMPONENT_LABEL), Some(roles));
        mandatory_labels.insert(
            String::from(APP_INSTANCE_LABEL),
            Some(vec![self.context.resource.name()]),
        );
        mandatory_labels.insert(
            String::from(APP_VERSION_LABEL),
            Some(vec![self.context.resource.spec.version.to_string()]),
        );
        mandatory_labels.insert(
            String::from(APP_NAME_LABEL),
            Some(vec![String::from(APP_NAME)]),
        );
        mandatory_labels.insert(
            String::from(APP_MANAGED_BY_LABEL),
            Some(vec![String::from(MANAGED_BY)]),
        );

        mandatory_labels
    }

    async fn delete_all_pods(&self) -> OperatorResult<ReconcileFunctionAction> {
        for pod in &self.existing_pods {
            self.context.client.delete(pod).await?;
        }
        Ok(ReconcileFunctionAction::Done)
    }

    /// Create or update a config map.
    /// - Create if no config map of that name exists
    /// - Update if config map exists but the content differs
    /// - Do nothing if the config map exists and the content is identical
    /// - Forward any kube errors that may appear
    // TODO: move to operator-rs
    async fn create_config_map(&self, config_map: ConfigMap) -> Result<(), Error> {
        let cm_name = match config_map.metadata.name.as_deref() {
            None => return Err(Error::InvalidConfigMap),
            Some(name) => name,
        };

        match self
            .context
            .client
            .get::<ConfigMap>(cm_name, Some(&self.context.namespace()))
            .await
        {
            Ok(ConfigMap {
                data: existing_config_map_data,
                ..
            }) if existing_config_map_data == config_map.data => {
                debug!(
                    "ConfigMap [{}] already exists with identical data, skipping creation!",
                    cm_name
                );
            }
            Ok(_) => {
                debug!(
                    "ConfigMap [{}] already exists, but differs, updating it!",
                    cm_name
                );
                self.context.client.update(&config_map).await?;
            }
            Err(stackable_operator::error::Error::KubeError {
                source: kube::error::Error::Api(ErrorResponse { reason, .. }),
            }) if reason == "NotFound" => {
                debug!("Error getting ConfigMap [{}]: [{:?}]", cm_name, reason);
                self.context.client.create(&config_map).await?;
            }
            Err(e) => return Err(Error::OperatorError { source: e }),
        }

        Ok(())
    }

    async fn create_missing_pods(&mut self) -> NifiReconcileResult {
        // The iteration happens in two stages here, to accommodate the way our operators think
        // about nodes and roles.
        // The hierarchy is:
        // - Roles (Nifi Node)
        //   - Role groups for this role (user defined)
        for role in NifiRole::iter() {
            let role_str = &role.to_string();
            if let Some(nodes_for_role) = self.eligible_nodes.get(role_str) {
                for (role_group, nodes) in nodes_for_role {
                    debug!(
                        "Identify missing pods for [{}] role and group [{}]",
                        role_str, role_group
                    );
                    trace!(
                        "candidate_nodes[{}]: [{:?}]",
                        nodes.len(),
                        nodes
                            .iter()
                            .map(|node| node.metadata.name.as_ref().unwrap())
                            .collect::<Vec<_>>()
                    );
                    trace!(
                        "existing_pods[{}]: [{:?}]",
                        &self.existing_pods.len(),
                        &self
                            .existing_pods
                            .iter()
                            .map(|pod| pod.metadata.name.as_ref().unwrap())
                            .collect::<Vec<_>>()
                    );
                    trace!(
                        "labels: [{:?}]",
                        get_role_and_group_labels(role_str, role_group)
                    );
                    let nodes_that_need_pods = k8s_utils::find_nodes_that_need_pods(
                        nodes,
                        &self.existing_pods,
                        &get_role_and_group_labels(role_str, role_group),
                    );

                    for node in nodes_that_need_pods {
                        let node_name = if let Some(node_name) = &node.metadata.name {
                            node_name
                        } else {
                            warn!("No name found in metadata, this should not happen! Skipping node: [{:?}]", node);
                            continue;
                        };
                        debug!(
                            "Creating pod on node [{}] for [{}] role and group [{}]",
                            node.metadata
                                .name
                                .as_deref()
                                .unwrap_or("<no node name found>"),
                            role,
                            role_group
                        );

                        let (pod, config_maps) = self
                            .create_pod_and_config_maps(
                                &role,
                                role_group,
                                &node_name,
                                config_for_role_and_group(
                                    role_str,
                                    role_group,
                                    &self.validated_role_config,
                                )?,
                            )
                            .await?;

                        for config_map in config_maps {
                            self.create_config_map(config_map).await?;
                        }

                        self.context.client.create(&pod).await?;
                    }
                }
            }
        }
        Ok(ReconcileFunctionAction::Continue)
    }

    async fn create_pod_and_config_maps(
        &self,
        role: &NifiRole,
        role_group: &str,
        node_name: &str,
        validated_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    ) -> Result<(Pod, Vec<ConfigMap>), Error> {
        let mut config_maps = vec![];
        let mut env_vars = vec![];
        let mut cm_data = BTreeMap::new();

        for (property_name_kind, config) in validated_config {
            // we need to convert to <String, String> to <String, Option<String>> to deal with
            // CLI flags etc. We can not currently represent that via operator-rs / product-config.
            // This is a preparation for that.
            let transformed_config: BTreeMap<String, Option<String>> = config
                .iter()
                .map(|(k, v)| (k.to_string(), Some(v.to_string())))
                .collect();

            let zk_connect_string = &self.zookeeper_info.as_ref().unwrap().connection_string;

            match property_name_kind {
                PropertyNameKind::File(file_name) => match file_name.as_str() {
                    config::NIFI_BOOTSTRAP_CONF => {
                        cm_data.insert(file_name.to_string(), build_bootstrap_conf());
                    }
                    config::NIFI_PROPERTIES => {
                        cm_data.insert(
                            file_name.to_string(),
                            // TODO: Improve the product config and properties handling here
                            //    now we "hardcode" the properties we require. NiFi has lots of
                            //    settings which we should process in a better manner.
                            build_nifi_properties(
                                &self.context.resource.spec,
                                config.get(NIFI_WEB_HTTP_PORT),
                                config.get(NIFI_CLUSTER_NODE_PROTOCOL_PORT),
                                config.get(NIFI_CLUSTER_LOAD_BALANCE_PORT),
                                zk_connect_string,
                                node_name,
                            ),
                        );
                    }
                    config::NIFI_STATE_MANAGEMENT_XML => {
                        cm_data.insert(
                            file_name.to_string(),
                            build_state_management_xml(
                                &self.context.resource.spec,
                                zk_connect_string,
                            ),
                        );
                    }
                    _ => {
                        warn!("Unknown filename [{}] was provided in product config. Possible values are {:?}", 
                              file_name, vec![config::NIFI_BOOTSTRAP_CONF, config::NIFI_PROPERTIES, config::NIFI_STATE_MANAGEMENT_XML]);
                    }
                },
                PropertyNameKind::Env => {
                    for (property_name, property_value) in transformed_config {
                        if property_name.is_empty() {
                            warn!("Received empty property_name for ENV... skipping");
                            continue;
                        }

                        env_vars.push(EnvVar {
                            name: property_name,
                            value: property_value,
                            value_from: None,
                        });
                    }
                }
                _ => {}
            }
        }

        let pod_name = pod_utils::get_pod_name(
            APP_NAME,
            &self.context.name(),
            role_group,
            &role.to_string(),
            node_name,
        );

        let cm_name = format!("{}-config", pod_name);

        let version = &self.context.resource.spec.version.to_string();

        let labels = get_recommended_labels(
            &self.context.resource,
            APP_NAME,
            version,
            &role.to_string(),
            role_group,
        );

        let mut container_builder = ContainerBuilder::new("nifi");
        container_builder.image(format!("nifi:{}", version));
        container_builder.command(build_nifi_start_command(&self.context.resource.spec));
        // TODO: For now we set the mount path to the NiFi package config folder.
        //   This needs to be investigated and changed into an separate config folder.
        //   Related to: https://issues.apache.org/jira/browse/NIFI-5573
        container_builder.add_configmapvolume(
            cm_name.clone(),
            format!("{}/nifi-{}/conf", "{{packageroot}}", version),
        );

        container_builder.add_env_vars(env_vars);

        let pod = PodBuilder::new()
            .metadata(
                ObjectMetaBuilder::new()
                    .name(pod_name)
                    .namespace(&self.context.client.default_namespace)
                    .with_labels(labels)
                    .ownerreference_from_resource(&self.context.resource, Some(true), Some(true))?
                    .build()?,
            )
            .add_stackable_agent_tolerations()
            .add_container(container_builder.build())
            .node_name(node_name)
            .build()?;

        config_maps.push(
            ConfigMapBuilder::new()
                .metadata(
                    ObjectMetaBuilder::new()
                        .name(cm_name)
                        .ownerreference_from_resource(
                            &self.context.resource,
                            Some(true),
                            Some(true),
                        )?
                        .namespace(&self.context.client.default_namespace)
                        .build()?,
                )
                .data(cm_data)
                .build()?,
        );

        Ok((pod, config_maps))
    }
}

impl ReconciliationState for NifiState {
    type Error = error::Error;

    fn reconcile(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<ReconcileFunctionAction, Self::Error>> + Send + '_>>
    {
        info!("========================= Starting reconciliation =========================");

        Box::pin(async move {
            self.context
                .handle_deletion(Box::pin(self.delete_all_pods()), FINALIZER_NAME, true)
                .await?
                .then(self.get_zookeeper_connection_information())
                .await?
                .then(self.context.delete_illegal_pods(
                    self.existing_pods.as_slice(),
                    &self.required_pod_labels(),
                    ContinuationStrategy::OneRequeue,
                ))
                .await?
                .then(
                    self.context
                        .wait_for_terminating_pods(self.existing_pods.as_slice()),
                )
                .await?
                .then(
                    self.context
                        .wait_for_running_and_ready_pods(&self.existing_pods.as_slice()),
                )
                .await?
                .then(self.context.delete_excess_pods(
                    list_eligible_nodes_for_role_and_group(&self.eligible_nodes).as_slice(),
                    self.existing_pods.as_slice(),
                    ContinuationStrategy::OneRequeue,
                ))
                .await?
                .then(self.create_missing_pods())
                .await
        })
    }
}

struct NifiStrategy {
    config: Arc<ProductConfigManager>,
}

impl NifiStrategy {
    pub fn new(config: ProductConfigManager) -> NifiStrategy {
        NifiStrategy {
            config: Arc::new(config),
        }
    }
}

#[async_trait]
impl ControllerStrategy for NifiStrategy {
    type Item = NifiCluster;
    type State = NifiState;
    type Error = error::Error;

    async fn init_reconcile_state(
        &self,
        context: ReconciliationContext<Self::Item>,
    ) -> Result<Self::State, Self::Error> {
        let existing_pods = context
            .list_owned(build_common_labels_for_all_managed_resources(
                APP_NAME,
                &context.resource.name(),
            ))
            .await?;
        trace!(
            "{}: Found [{}] pods",
            context.log_name(),
            existing_pods.len()
        );

        let nifi_spec: NifiSpec = context.resource.spec.clone();
        let mut eligible_nodes = HashMap::new();

        eligible_nodes.insert(
            NifiRole::Node.to_string(),
            role_utils::find_nodes_that_fit_selectors(&context.client, None, &nifi_spec.nodes)
                .await?,
        );

        Ok(NifiState {
            validated_role_config: validated_product_config(&context.resource, &self.config)?,
            context,
            existing_pods,
            eligible_nodes,
            zookeeper_info: None,
        })
    }
}

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
pub async fn create_controller(client: Client) {
    let nifi_api: Api<NifiCluster> = client.get_all_api();
    let pods_api: Api<Pod> = client.get_all_api();
    let configmaps_api: Api<ConfigMap> = client.get_all_api();

    let controller = Controller::new(nifi_api)
        .owns(pods_api, ListParams::default())
        .owns(configmaps_api, ListParams::default());

    let product_config =
        ProductConfigManager::from_yaml_file("deploy/config-spec/properties.yaml").unwrap();
    let strategy = NifiStrategy::new(product_config);

    controller
        .run(client, strategy, Duration::from_secs(10))
        .await;
}

/// Retrieve the config belonging to a role group selector.
///
/// # Arguments
/// * `spec` - The custom resource spec definition to extract the version
///
fn build_nifi_start_command(spec: &NifiSpec) -> Vec<String> {
    vec![format!("nifi-{}/bin/nifi.sh run", spec.version.to_string())]
}
