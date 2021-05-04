mod config;
mod error;

use crate::config::{create_bootstrap_conf, create_nifi_properties, create_state_management_xml};
use crate::error::Error;
use async_trait::async_trait;
use futures::Future;
use k8s_openapi::api::core::v1::{
    ConfigMap, ConfigMapVolumeSource, Container, Node, Pod, PodSpec, Volume, VolumeMount,
};
use kube::api::ListParams;
use kube::Api;
use stackable_nifi_crd::{NifiCluster, NifiConfig, NifiSpec, ZookeeperReference};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::k8s_utils::LabelOptionalValueMap;
use stackable_operator::labels::{
    APP_COMPONENT_LABEL, APP_INSTANCE_LABEL, APP_ROLE_GROUP_LABEL, APP_VERSION_LABEL,
};
use stackable_operator::reconcile::{
    ContinuationStrategy, ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use stackable_operator::role_utils::RoleGroup;
use stackable_operator::{k8s_utils, metadata, role_utils};
use stackable_zookeeper_crd::util::{get_zk_connection_info, is_valid_node};
use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::time::Duration;
use strum::IntoEnumIterator;
use strum_macros::Display;
use strum_macros::EnumIter;
use tracing::{debug, info, trace, warn};

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

    async fn create_config_map(&self, name: &str, config: &NifiConfig) -> Result<(), Error> {
        match self
            .context
            .client
            .get::<ConfigMap>(name, Some(&"default".to_string()))
            .await
        {
            // TODO: check and compare content here
            Ok(_) => {
                debug!("ConfigMap [{}] already exists, skipping creation!", name);
                return Ok(());
            }
            Err(e) => {
                // TODO: This is shit, but works for now. If there is an actual error in comes with
                //   K8S, it will most probably also occur further down and be properly handled
                debug!("Error getting ConfigMap [{}]: [{:?}]", name, e);
            }
        }

        let zk_ref: &ZookeeperReference = &self.context.resource.spec.zookeeper_reference;

        // retrieve zookeeper connect string
        let zookeeper_info =
            get_zk_connection_info(&self.context.client, &zk_ref.name, &zk_ref.namespace, None)
                .await?;

        if let Some(chroot) = zk_ref.chroot.as_deref() {
            is_valid_node(chroot)?;
        }

        info!(
            "Found zookeeper reference: {}",
            &zookeeper_info.connection_string
        );

        let bootstrap_conf = create_bootstrap_conf();
        let nifi_properties = create_nifi_properties(
            &self.context.resource.spec,
            config,
            &zookeeper_info.connection_string,
        );
        let state_management_xml = create_state_management_xml(
            &self.context.resource.spec,
            &zookeeper_info.connection_string,
        );

        let mut data = BTreeMap::new();
        data.insert("bootstrap.conf".to_string(), bootstrap_conf);
        data.insert("nifi.properties".to_string(), nifi_properties);
        data.insert("state-management.xml".to_string(), state_management_xml);

        // And now create the actual ConfigMap
        let cm =
            stackable_operator::config_map::create_config_map(&self.context.resource, &name, data)?;
        self.context.client.create(&cm).await?;
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
                    // extract selector for nifi config
                    let nifi_config = match self
                        .context
                        .resource
                        .spec
                        .nodes
                        .selectors
                        .get(role_group)
                    {
                        Some(selector) => &selector.config,
                        None => {
                            warn!(
                                    "No config found in selector for role [{}], this should not happen!",
                                    &role_group
                                );
                            continue;
                        }
                    };

                    // Create config map for this role group
                    let pod_name =
                        format!("nifi-{}-{}-{}", self.context.name(), role_group, nifi_role)
                            .to_lowercase();

                    let cm_name = format!("{}-config", pod_name);
                    debug!("pod_name: [{}], cm_name: [{}]", pod_name, cm_name);

                    self.create_config_map(&cm_name, nifi_config).await?;

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

                        let mut node_labels = BTreeMap::new();
                        node_labels
                            .insert(String::from(APP_COMPONENT_LABEL), nifi_role.to_string());
                        node_labels
                            .insert(String::from(APP_ROLE_GROUP_LABEL), String::from(role_group));
                        node_labels.insert(String::from(APP_INSTANCE_LABEL), self.context.name());
                        node_labels.insert(
                            String::from(APP_VERSION_LABEL),
                            self.context.resource.spec.version.to_string(),
                        );

                        // Create a pod for this node, role and group combination
                        let pod = build_pod(
                            &self.context.resource,
                            node_name,
                            &node_labels,
                            &pod_name,
                            &cm_name,
                            nifi_config,
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

fn build_pod(
    resource: &NifiCluster,
    node: &str,
    labels: &BTreeMap<String, String>,
    pod_name: &str,
    cm_name: &str,
    config: &NifiConfig,
) -> Result<Pod, Error> {
    let pod = Pod {
        metadata: metadata::build_metadata(
            pod_name.to_string(),
            Some(labels.clone()),
            resource,
            false,
        )?,
        spec: Some(PodSpec {
            node_name: Some(node.to_string()),
            tolerations: Some(stackable_operator::krustlet::create_tolerations()),
            containers: vec![Container {
                image: Some(format!("nifi:{}", resource.spec.version.to_string())),
                name: "nifi".to_string(),
                command: Some(create_nifi_start_command(&resource.spec, config)),
                volume_mounts: Some(vec![VolumeMount {
                    mount_path: format!(
                        "{}/nifi-{}/conf",
                        "{{packageroot}}",
                        resource.spec.version.to_string()
                    ),
                    name: "config-volume".to_string(),
                    ..VolumeMount::default()
                }]),
                ..Container::default()
            }],
            volumes: Some(vec![Volume {
                name: "config-volume".to_string(),
                config_map: Some(ConfigMapVolumeSource {
                    name: Some(cm_name.to_string()),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            }]),
            ..PodSpec::default()
        }),
        ..Pod::default()
    };
    Ok(pod)
}

fn create_nifi_start_command(spec: &NifiSpec, _config: &NifiConfig) -> Vec<String> {
    let command = vec![format!("nifi-{}/bin/nifi.sh run", spec.version.to_string())];
    command
}
