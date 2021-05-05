mod config;
mod error;
mod pod_utils;

use crate::config::{create_bootstrap_conf, create_nifi_properties, create_state_management_xml};
use crate::error::Error;
use crate::pod_utils::build_labels;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::{ConfigMap, Node, Pod};
use kube::api::ListParams;
use kube::Api;
use stackable_nifi_crd::{NifiCluster, NifiConfig, NifiSpec, ZookeeperReference};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::k8s_utils::LabelOptionalValueMap;
use stackable_operator::labels::{APP_COMPONENT_LABEL, APP_INSTANCE_LABEL, APP_ROLE_GROUP_LABEL};
use stackable_operator::reconcile::{
    ContinuationStrategy, ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use stackable_operator::role_utils::RoleGroup;
use stackable_operator::{k8s_utils, role_utils};
use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::time::Duration;
use strum::IntoEnumIterator;
use strum_macros::Display;
use strum_macros::EnumIter;
use tracing::{debug, error, info, trace, warn};

type NifiReconcileResult = ReconcileResult<error::Error>;

#[derive(EnumIter, Debug, Display, PartialEq, Eq, Hash)]
pub enum NifiRole {
    Node,
}

struct NifiState {
    context: ReconciliationContext<NifiCluster>,
    existing_pods: Vec<Pod>,
    eligible_nodes: HashMap<NifiRole, HashMap<String, Vec<Node>>>,
}

impl NifiState {
    pub fn get_full_pod_node_map(&self) -> Vec<(Vec<Node>, LabelOptionalValueMap)> {
        let mut eligible_nodes_map = vec![];

        for node_type in NifiRole::iter() {
            if let Some(eligible_nodes_for_role) = self.eligible_nodes.get(&node_type) {
                for (group_name, eligible_nodes) in eligible_nodes_for_role {
                    // Create labels to identify eligible nodes
                    trace!(
                        "Adding [{}] nodes to eligible node list for role [{}] and group [{}].",
                        eligible_nodes.len(),
                        node_type,
                        group_name
                    );
                    eligible_nodes_map.push((
                        eligible_nodes.clone(),
                        get_node_and_group_labels(group_name, &node_type),
                    ))
                }
            }
        }
        eligible_nodes_map
    }

    pub fn get_deletion_labels(&self) -> BTreeMap<String, Option<Vec<String>>> {
        let roles = NifiRole::iter()
            .map(|role| role.to_string())
            .collect::<Vec<_>>();
        let mut mandatory_labels = BTreeMap::new();

        mandatory_labels.insert(String::from(APP_COMPONENT_LABEL), Some(roles));
        mandatory_labels.insert(String::from(APP_INSTANCE_LABEL), None);
        mandatory_labels
    }

    async fn create_config_map(
        &self,
        cm_name: &str,
        config: &NifiConfig,
        node_name: &str,
    ) -> Result<(), Error> {
        let zk_ref: &ZookeeperReference = &self.context.resource.spec.zookeeper_reference;

        // retrieve zookeeper connect string
        let zookeeper_info = stackable_zookeeper_crd::util::get_zk_connection_info(
            &self.context.client,
            &zk_ref.name,
            &zk_ref.namespace,
            None,
        )
        .await?;

        if let Some(chroot) = zk_ref.chroot.as_deref() {
            stackable_zookeeper_crd::util::is_valid_node(chroot)?;
        }

        debug!(
            "Received zookeeper connect string: [{}]",
            &zookeeper_info.connection_string
        );

        let bootstrap_conf = create_bootstrap_conf();
        let nifi_properties = create_nifi_properties(
            &self.context.resource.spec,
            config,
            &zookeeper_info.connection_string,
            node_name,
        );
        let state_management_xml = create_state_management_xml(
            &self.context.resource.spec,
            &zookeeper_info.connection_string,
        );

        let mut data = BTreeMap::new();
        data.insert("bootstrap.conf".to_string(), bootstrap_conf);
        data.insert("nifi.properties".to_string(), nifi_properties);
        data.insert("state-management.xml".to_string(), state_management_xml);

        match self
            .context
            .client
            .get::<ConfigMap>(cm_name, Some(&"default".to_string()))
            .await
        {
            Ok(config_map) => {
                if let Some(existing_config_map_data) = config_map.data {
                    if existing_config_map_data == data {
                        debug!(
                            "ConfigMap [{}] already exists with identical data, skipping creation!",
                            cm_name
                        );
                        return Ok(());
                    } else {
                        debug!(
                            "ConfigMap [{}] already exists, but differs, recreating it!",
                            cm_name
                        );
                    }
                }
            }
            Err(e) => {
                // TODO: This is shit, but works for now. If there is an actual error in comes with
                //   K8S, it will most probably also occur further down and be properly handled
                debug!("Error getting ConfigMap [{}]: [{:?}]", cm_name, e);
            }
        }

        let cm = stackable_operator::config_map::create_config_map(
            &self.context.resource,
            &cm_name,
            data,
        )?;
        self.context.client.create(&cm).await?;
        Ok(())
    }

    async fn wait_for_host_name_and_create_config_map(&self) -> NifiReconcileResult {
        for pod in &self.existing_pods {
            let pod_name = match &pod.metadata.name {
                Some(name) => name,
                None => {
                    error!("Pod has no name set. This should not happen!");
                    return Ok(ReconcileFunctionAction::Done);
                }
            };

            let spec = match &pod.spec {
                Some(spec) => spec,
                None => {
                    debug!("Pod [{}] has no spec set yet. Waiting...!", &pod_name);
                    return Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(10)));
                }
            };

            let node_name = match &spec.node_name {
                Some(node_name) => node_name,
                None => {
                    debug!("Pod [{}] has no node_name set yet. Waiting...", &pod_name);
                    return Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(10)));
                }
            };

            let role_group = match pod_utils::get_pod_label(pod, APP_ROLE_GROUP_LABEL) {
                Some(role_group) => role_group,
                None => {
                    error!(
                        "Pod is missing label [{}]. This is a bug and should not happen!",
                        APP_ROLE_GROUP_LABEL
                    );
                    continue;
                }
            };

            match pod_utils::get_selector_config(&role_group, &self.context.resource.spec) {
                Some(config) => {
                    let cm_name = format!("{}-config", &pod_name);
                    debug!("Creating config map [{}] for pod [{}]", cm_name, pod_name);
                    self.create_config_map(&cm_name, &config, node_name).await?
                }
                None => {
                    error!("Role group [{}] does not have a config. This is a bug and should not happen!", role_group);
                    continue;
                }
            };
        }

        Ok(ReconcileFunctionAction::Continue)
    }

    async fn create_missing_pods(&mut self) -> NifiReconcileResult {
        // The iteration happens in two stages here, to accommodate the way our operators think
        // about nodes and roles.
        // The hierarchy is:
        // - Roles (for example Datanode, Namenode, Nifi Node)
        //   - Role groups for this role (user defined)
        for nifi_role in NifiRole::iter() {
            if let Some(nodes_for_role) = self.eligible_nodes.get(&nifi_role) {
                for (role_group, nodes) in nodes_for_role {
                    debug!(
                        "Identify missing pods for [{}] role and group [{}]",
                        nifi_role, role_group
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
                        get_node_and_group_labels(role_group, &nifi_role)
                    );
                    let nodes_that_need_pods = k8s_utils::find_nodes_that_need_pods(
                        nodes,
                        &self.existing_pods,
                        &get_node_and_group_labels(role_group, &nifi_role),
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
                            nifi_role,
                            role_group
                        );

                        let version = self.context.resource.spec.version.to_string();
                        let labels =
                            build_labels(&nifi_role, role_group, &self.context.name(), &version);

                        let pod_name =
                            format!("nifi-{}-{}-{}", self.context.name(), role_group, nifi_role)
                                .to_lowercase();

                        // Create config map for this role group
                        let cm_name = format!("{}-config", pod_name);
                        debug!("pod_name: [{}], cm_name: [{}]", pod_name, cm_name);

                        // Create a pod for this node, role and group combination
                        let pod = pod_utils::build_pod(
                            &self.context.resource,
                            node_name,
                            &pod_name,
                            &cm_name,
                            &version,
                            labels,
                        )?;

                        self.context.client.create(&pod).await?;
                    }
                }
            }
        }
        Ok(ReconcileFunctionAction::Continue)
    }
}

impl ReconciliationState for NifiState {
    type Error = error::Error;

    fn reconcile(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<ReconcileFunctionAction, Self::Error>> + Send + '_>>
    {
        info!("========================= Starting reconciliation =========================");
        debug!("Deletion Labels: [{:?}]", &self.get_deletion_labels());

        Box::pin(async move {
            self.context
                .delete_illegal_pods(
                    self.existing_pods.as_slice(),
                    &self.get_deletion_labels(),
                    ContinuationStrategy::OneRequeue,
                )
                .await?
                .then(self.wait_for_host_name_and_create_config_map())
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
                    self.get_full_pod_node_map().as_slice(),
                    self.existing_pods.as_slice(),
                    ContinuationStrategy::OneRequeue,
                ))
                .await?
                .then(self.create_missing_pods())
                .await
        })
    }
}

#[derive(Debug)]
struct NifiStrategy {}

impl NifiStrategy {
    pub fn new() -> NifiStrategy {
        NifiStrategy {}
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
        let existing_pods = context.list_pods().await?;
        trace!("Found [{}] pods", existing_pods.len());

        let nifi_spec: NifiSpec = context.resource.spec.clone();

        let mut eligible_nodes = HashMap::new();

        let role_groups: Vec<RoleGroup> = nifi_spec
            .nodes
            .selectors
            .iter()
            .map(|(group_name, selector_config)| RoleGroup {
                name: group_name.to_string(),
                selector: selector_config.clone().selector.unwrap(),
            })
            .collect();

        eligible_nodes.insert(
            NifiRole::Node,
            role_utils::find_nodes_that_fit_selectors(
                &context.client,
                None,
                role_groups.as_slice(),
            )
            .await?,
        );

        Ok(NifiState {
            context,
            existing_pods,
            eligible_nodes,
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

    let strategy = NifiStrategy::new();

    controller
        .run(client, strategy, Duration::from_secs(10))
        .await;
}

fn get_node_and_group_labels(group_name: &str, role: &NifiRole) -> LabelOptionalValueMap {
    let mut node_labels = BTreeMap::new();
    node_labels.insert(String::from(APP_COMPONENT_LABEL), Some(role.to_string()));
    node_labels.insert(
        String::from(APP_ROLE_GROUP_LABEL),
        Some(String::from(group_name)),
    );
    node_labels
}
