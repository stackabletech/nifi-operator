mod config;
mod error;
mod pod_utils;

use crate::config::{build_bootstrap_conf, build_nifi_properties, build_state_management_xml};
use crate::error::Error;
use crate::pod_utils::build_labels;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::{ConfigMap, Node, Pod};
use kube::api::ListParams;
use kube::Api;
use stackable_nifi_crd::{NifiCluster, NifiConfig, NifiSpec};
use stackable_operator::client::Client;
use stackable_operator::config_map;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::k8s_utils::LabelOptionalValueMap;
use stackable_operator::labels;
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

        mandatory_labels.insert(String::from(labels::APP_COMPONENT_LABEL), Some(roles));
        mandatory_labels.insert(String::from(labels::APP_INSTANCE_LABEL), None);
        mandatory_labels
    }

    /// Builds and checks the existence of a config map in multiple steps.
    /// 1) Validate the provided ZookeeperReference from the custom resource
    /// 2) Retrieve the ZooKeeper connection string
    /// 3) Create the desired config map data using the config properties and ZooKeeper connection string
    /// 4) Check if a config map with identical name exists
    ///     * if so compare the data content of the created and existing config map
    ///         - if identical do nothing
    ///         - if different, create the config map with new data and update
    ///     * if no config map found, create the config map with the new data
    ///  
    async fn create_config_map(
        &self,
        cm_name: &str,
        config: &NifiConfig,
        node_name: &str,
    ) -> Result<(), Error> {
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

        let bootstrap_conf = build_bootstrap_conf();
        let nifi_properties = build_nifi_properties(
            &self.context.resource.spec,
            config,
            &zookeeper_info.connection_string,
            node_name,
        );
        let state_management_xml = build_state_management_xml(
            &self.context.resource.spec,
            &zookeeper_info.connection_string,
        );

        let mut data = BTreeMap::new();
        data.insert("bootstrap.conf".to_string(), bootstrap_conf);
        data.insert("nifi.properties".to_string(), nifi_properties);
        data.insert("state-management.xml".to_string(), state_management_xml);

        // And now create the actual ConfigMap
        let config_map = config_map::create_config_map(&self.context.resource, &cm_name, data)?;

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
            Err(e) => {
                // TODO: This is shit, but works for now. If there is an actual error in comes with
                //   K8S, it will most probably also occur further down and be properly handled
                debug!("Error getting ConfigMap [{}]: [{:?}]", cm_name, e);
                self.context.client.create(&config_map).await?;
            }
        }

        Ok(())
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

                        let labels = build_labels(
                            &nifi_role,
                            role_group,
                            &self.context.name(),
                            &self.context.resource.spec.version.to_string(),
                        );

                        let pod_name = format!(
                            "nifi-{}-{}-{}-{}",
                            self.context.name(),
                            role_group,
                            nifi_role,
                            node_name
                        )
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
                            labels,
                        )?;

                        self.context.client.create(&pod).await?;

                        // we need a NifiConfig to create proper config map data
                        if let Some(config) =
                            pod_utils::get_selector_config(&role_group, &self.context.resource.spec)
                        {
                            self.create_config_map(&cm_name, &config, node_name).await?;
                        } else {
                            error!("Role group [{}] does not have a config. This is a bug and should not happen!", role_group);
                        }
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
    node_labels.insert(
        String::from(labels::APP_COMPONENT_LABEL),
        Some(role.to_string()),
    );
    node_labels.insert(
        String::from(labels::APP_ROLE_GROUP_LABEL),
        Some(String::from(group_name)),
    );
    node_labels
}
