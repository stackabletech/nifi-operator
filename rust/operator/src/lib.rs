mod config;
mod error;
mod monitoring;

use crate::config::{
    build_bootstrap_conf, build_nifi_properties, build_state_management_xml,
    validated_product_config,
};
use crate::monitoring::{
    NifiRestClient, ReportingTask, ReportingTaskState, ReportingTaskStatus, NO_TASK_ID,
};

use async_trait::async_trait;
use futures::Future;
use stackable_nifi_crd::{
    NifiCluster, NifiRole, NifiSpec, APP_NAME, MANAGED_BY, NIFI_CLUSTER_LOAD_BALANCE_PORT,
    NIFI_CLUSTER_METRICS_PORT, NIFI_CLUSTER_NODE_PROTOCOL_PORT, NIFI_WEB_HTTP_PORT,
};
use stackable_operator::builder::{ContainerBuilder, ObjectMetaBuilder, PodBuilder, VolumeBuilder};
use stackable_operator::client::Client;
use stackable_operator::controller::{Controller, ControllerStrategy, ReconciliationState};
use stackable_operator::error::OperatorResult;
use stackable_operator::identity::{
    LabeledPodIdentityFactory, NodeIdentity, PodIdentity, PodToNodeMapping,
};
use stackable_operator::k8s_openapi::api::core::v1::{ConfigMap, EnvVar, Pod};
use stackable_operator::kube::api::ListParams;
use stackable_operator::kube::Api;
use stackable_operator::kube::ResourceExt;
use stackable_operator::labels::{
    build_common_labels_for_all_managed_resources, get_recommended_labels, APP_COMPONENT_LABEL,
    APP_INSTANCE_LABEL, APP_MANAGED_BY_LABEL, APP_NAME_LABEL, APP_VERSION_LABEL,
};
use stackable_operator::product_config::types::PropertyNameKind;
use stackable_operator::product_config::ProductConfigManager;
use stackable_operator::product_config_utils::{
    config_for_role_and_group, ValidatedRoleConfigByPropertyKind,
};
use stackable_operator::reconcile::{
    ContinuationStrategy, ReconcileFunctionAction, ReconcileResult, ReconciliationContext,
};
use stackable_operator::role_utils::{
    get_role_and_group_labels, list_eligible_nodes_for_role_and_group, EligibleNodesForRoleAndGroup,
};
use stackable_operator::scheduler::{
    K8SUnboundedHistory, RoleGroupEligibleNodes, ScheduleStrategy, Scheduler, StickyScheduler,
};
use stackable_operator::status::init_status;
use stackable_operator::versioning::{finalize_versioning, init_versioning};
use stackable_operator::{configmap, name_utils, role_utils};
use stackable_zookeeper_crd::util::ZookeeperConnectionInformation;
use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use strum::IntoEnumIterator;
use tracing::{debug, info, trace, warn};

const FINALIZER_NAME: &str = "nifi.stackable.tech/cleanup";
const SHOULD_BE_SCRAPED: &str = "monitoring.stackable.tech/should_be_scraped";

const HTTP_PORT_NAME: &str = "http";
const PROTOCOL_PORT_NAME: &str = "protocol";
const LOAD_BALANCE_PORT_NAME: &str = "loadbalance";
const METRICS_PORT_NAME: &str = "metrics";
const CONFIG_MAP_TYPE_CONFIG: &str = "config";
const ID_LABEL: &str = "monitoring.stackable.tech/id";

const STACKABLE_TMP_CONFIG: &str = "/stackable/tmp-conf";

type NifiReconcileResult = ReconcileResult<error::NifiError>;

struct NifiState {
    context: ReconciliationContext<NifiCluster>,
    eligible_nodes: EligibleNodesForRoleAndGroup,
    existing_pods: Vec<Pod>,
    monitoring: Arc<NifiRestClient>,
    validated_role_config: ValidatedRoleConfigByPropertyKind,
    zookeeper_info: Option<ZookeeperConnectionInformation>,
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

    /// Will initialize the status object if it's never been set.
    async fn init_status(&mut self) -> NifiReconcileResult {
        // init status with default values if not available yet.
        self.context.resource = init_status(&self.context.client, &self.context.resource).await?;

        let spec_version = self.context.resource.spec.version.clone();

        self.context.resource =
            init_versioning(&self.context.client, &self.context.resource, spec_version).await?;

        Ok(ReconcileFunctionAction::Continue)
    }

    async fn delete_all_pods(&self) -> OperatorResult<ReconcileFunctionAction> {
        for pod in &self.existing_pods {
            self.context.client.delete(pod).await?;
        }
        Ok(ReconcileFunctionAction::Done)
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
                for (role_group, eligible_nodes) in nodes_for_role {
                    debug!(
                        "Identify missing pods for [{}] role and group [{}]",
                        role_str, role_group
                    );
                    trace!(
                        "candidate_nodes[{}]: [{:?}]",
                        eligible_nodes.nodes.len(),
                        eligible_nodes
                            .nodes
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

                    let mut history = match self
                        .context
                        .resource
                        .status
                        .as_ref()
                        .and_then(|status| status.history.as_ref())
                    {
                        Some(simple_history) => {
                            // we clone here because we cannot access mut self because we need it later
                            // to create config maps and pods. The `status` history will be out of sync
                            // with the cloned `simple_history` until the next reconcile.
                            // The `status` history should not be used after this method to avoid side
                            // effects.
                            K8SUnboundedHistory::new(&self.context.client, simple_history.clone())
                        }
                        None => K8SUnboundedHistory::new(
                            &self.context.client,
                            PodToNodeMapping::default(),
                        ),
                    };

                    let mut sticky_scheduler =
                        StickyScheduler::new(&mut history, ScheduleStrategy::GroupAntiAffinity);

                    let pod_id_factory = LabeledPodIdentityFactory::new(
                        APP_NAME,
                        &self.context.name(),
                        &self.eligible_nodes,
                        ID_LABEL,
                        1,
                    );

                    let state = sticky_scheduler.schedule(
                        &pod_id_factory,
                        &RoleGroupEligibleNodes::from(&self.eligible_nodes),
                        &self.existing_pods,
                    )?;

                    let mapping = state.remaining_mapping().filter(
                        APP_NAME,
                        &self.context.name(),
                        role_str,
                        role_group,
                    );

                    if let Some((pod_id, node_id)) = mapping.iter().next() {
                        // now we have a node that needs a pod -> get validated config
                        let validated_config = config_for_role_and_group(
                            pod_id.role(),
                            pod_id.group(),
                            &self.validated_role_config,
                        )?;

                        let config_maps = self
                            .create_config_maps(pod_id, node_id, validated_config)
                            .await?;

                        self.create_pod(pod_id, node_id, &config_maps, validated_config)
                            .await?;

                        history.save(&self.context.resource).await?;

                        return Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(10)));
                    }
                }
            }
        }

        // If we reach here it means all pods must be running on target_version.
        // We can now set current_version to target_version (if target_version was set) and
        // target_version to None
        finalize_versioning(&self.context.client, &self.context.resource).await?;

        Ok(ReconcileFunctionAction::Continue)
    }

    /// Creates the config maps required for a NiFi instance (or role, role_group combination):
    /// * 'bootstrap.conf'
    /// * 'nifi.properties'
    /// * 'state-management.xml'
    ///
    /// These three configuration files are collected in one config map for now.
    ///
    /// Labels are automatically adapted from the `recommended_labels` with a type (bootstrap,
    /// properties, state-management). Names are generated via `name_utils::build_resource_name`.
    ///
    /// Returns a map with a 'type' identifier (e.g. bootstrap) as key and the corresponding
    /// ConfigMap as value. This is required to set the volume mounts in the pod later on.
    ///
    /// # Arguments
    ///
    /// - `pod_id` - The pod id for which to create config maps.
    /// - `node_id` - The node id where the pod will be placed.
    /// - `validated_config` - The validated product config.
    ///
    async fn create_config_maps(
        &self,
        pod_id: &PodIdentity,
        node_id: &NodeIdentity,
        validated_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    ) -> Result<HashMap<&'static str, ConfigMap>, error::NifiError> {
        let mut config_maps = HashMap::new();
        let mut cm_data = BTreeMap::new();

        let mut cm_labels = get_recommended_labels(
            &self.context.resource,
            pod_id.app(),
            &self.context.resource.spec.version.to_string(),
            pod_id.role(),
            pod_id.group(),
        );

        for (property_name_kind, config) in validated_config {
            let zk_connect_string = match self.zookeeper_info.as_ref() {
                Some(info) => &info.connection_string,
                None => return Err(error::NifiError::ZookeeperConnectionInformationError),
            };

            // enhance with config map type label
            cm_labels.insert(
                configmap::CONFIGMAP_TYPE_LABEL.to_string(),
                CONFIG_MAP_TYPE_CONFIG.to_string(),
            );

            if let PropertyNameKind::File(file_name) = property_name_kind {
                match file_name.as_str() {
                    config::NIFI_BOOTSTRAP_CONF => {
                        cm_data.insert(file_name.to_string(), build_bootstrap_conf());
                    }
                    config::NIFI_PROPERTIES => {
                        let http_port = config.get(NIFI_WEB_HTTP_PORT);
                        let protocol_port = config.get(NIFI_CLUSTER_NODE_PROTOCOL_PORT);
                        let load_balance = config.get(NIFI_CLUSTER_LOAD_BALANCE_PORT);

                        cm_data.insert(
                            file_name.to_string(),
                            // TODO: Improve the product config and properties handling here
                            //    now we "hardcode" the properties we require. NiFi has lots of
                            //    settings which we should process in a better manner.
                            build_nifi_properties(
                                &self.context.resource.spec,
                                http_port,
                                protocol_port,
                                load_balance,
                                zk_connect_string,
                                node_id.name.as_str(),
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
                }
            }
        }

        let cm_properties_name = name_utils::build_resource_name(
            pod_id.app(),
            pod_id.instance(),
            pod_id.role(),
            Some(pod_id.group()),
            Some(node_id.name.as_ref()),
            Some(CONFIG_MAP_TYPE_CONFIG),
        )?;

        let cm_config = configmap::build_config_map(
            &self.context.resource,
            &cm_properties_name,
            &self.context.namespace(),
            cm_labels,
            cm_data,
        )?;

        config_maps.insert(
            CONFIG_MAP_TYPE_CONFIG,
            configmap::create_config_map(&self.context.client, cm_config).await?,
        );

        Ok(config_maps)
    }

    /// Creates the pod required for the NiFi instance.
    ///
    /// # Arguments
    ///
    /// - `pod_id` - The id of the pod to create.
    /// - `node_id` - The id of the node where the pod will be placed.
    /// - `config_maps` - The config maps and respective types required for this pod.
    /// - `validated_config` - The validated product config.
    ///
    async fn create_pod(
        &self,
        pod_id: &PodIdentity,
        node_id: &NodeIdentity,
        config_maps: &HashMap<&'static str, ConfigMap>,
        validated_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    ) -> Result<Pod, error::NifiError> {
        let mut env_vars = vec![];
        let mut http_port: Option<&String> = None;
        let mut protocol_port: Option<&String> = None;
        let mut load_balance: Option<&String> = None;
        let mut metrics_port: Option<String> = None;

        let version = &self.context.resource.spec.version.to_string();

        // extract container ports from config
        if let Some(config) =
            validated_config.get(&PropertyNameKind::File(config::NIFI_PROPERTIES.to_string()))
        {
            http_port = config.get(NIFI_WEB_HTTP_PORT);
            protocol_port = config.get(NIFI_CLUSTER_NODE_PROTOCOL_PORT);
            load_balance = config.get(NIFI_CLUSTER_LOAD_BALANCE_PORT);
        }

        // extract metric port and env variables from env
        if let Some(config) = validated_config.get(&PropertyNameKind::Env) {
            for (property_name, property_value) in config {
                if property_name.is_empty() {
                    warn!("Received empty property_name for ENV... skipping");
                    continue;
                }

                if property_name == NIFI_CLUSTER_METRICS_PORT {
                    metrics_port = Some(property_value.clone());
                    continue;
                }

                env_vars.push(EnvVar {
                    name: property_name.clone(),
                    value: Some(property_value.clone()),
                    value_from: None,
                });
            }
        }

        let pod_name = name_utils::build_resource_name(
            pod_id.app(),
            pod_id.instance(),
            pod_id.role(),
            Some(pod_id.group()),
            Some(node_id.name.as_str()),
            None,
        )?;

        let mut labels = get_recommended_labels(
            &self.context.resource,
            pod_id.app(),
            version,
            pod_id.role(),
            pod_id.group(),
        );
        labels.insert(String::from(ID_LABEL), String::from(pod_id.id()));

        let mut container_builder = ContainerBuilder::new(APP_NAME);
        container_builder.image(format!(
            "docker.stackable.tech/stackable/nifi:{}-0.1",
            version
        ));
        container_builder.command(vec!["/bin/bash".to_string(), "-c".to_string()]);
        // we use the copy_assets.sh script here to copy everything from the "STACKABLE_TMP_CONFIG"
        // folder to the "conf" folder in the nifi package.
        container_builder.args(vec![format!(
            "/stackable/bin/copy_assets {} {}; {} {}",
            STACKABLE_TMP_CONFIG, version, "bin/nifi.sh", "run"
        )]);
        container_builder.add_env_vars(env_vars);

        let mut pod_builder = PodBuilder::new();

        // One mount for the config directory
        if let Some(config_map_data) = config_maps.get(CONFIG_MAP_TYPE_CONFIG) {
            if let Some(name) = config_map_data.metadata.name.as_ref() {
                container_builder.add_volume_mount("config", STACKABLE_TMP_CONFIG);
                pod_builder.add_volume(VolumeBuilder::new("config").with_config_map(name).build());
            } else {
                return Err(error::NifiError::MissingConfigMapNameError {
                    cm_type: CONFIG_MAP_TYPE_CONFIG,
                });
            }
        } else {
            return Err(error::NifiError::MissingConfigMapError {
                cm_type: CONFIG_MAP_TYPE_CONFIG,
                pod_name,
            });
        }

        if let Some(port) = http_port {
            container_builder.add_container_port(HTTP_PORT_NAME, port.parse()?);
        }

        if let Some(port) = protocol_port {
            container_builder.add_container_port(PROTOCOL_PORT_NAME, port.parse()?);
        }

        if let Some(port) = load_balance {
            container_builder.add_container_port(LOAD_BALANCE_PORT_NAME, port.parse()?);
        }

        let mut annotations = BTreeMap::new();
        if let Some(port) = metrics_port {
            // only add metrics container port and annotation if available
            annotations.insert(SHOULD_BE_SCRAPED.to_string(), "true".to_string());
            container_builder.add_container_port(METRICS_PORT_NAME, port.parse()?);
        }

        let pod = pod_builder
            .metadata(
                ObjectMetaBuilder::new()
                    .generate_name(pod_name)
                    .namespace(&self.context.client.default_namespace)
                    .with_labels(labels)
                    .with_annotations(annotations)
                    .ownerreference_from_resource(&self.context.resource, Some(true), Some(true))?
                    .build()?,
            )
            .add_container(container_builder.build())
            .node_name(node_id.name.as_str())
            // TODO: first iteration we are using host network
            .host_network(true)
            .build()?;

        Ok(self.context.client.create(&pod).await?)
    }

    /// In order to enable / disable monitoring for NiFi, we have to make several REST calls.
    /// There will be only one ReportingTask for the whole cluster. The task will be synced
    /// for all nodes.
    /// We always iterate over all the <node_name>:<http_port> pod combinations in order to
    /// make sure that network problems etc. will not affect this. Usually the first pod
    /// should be sufficient.
    /// ```ignore
    /// +-------------------------------------------------------------------------------+
    /// |         "StackablePrometheusReportingTask" available?                         |
    /// |            <no> |                          | <yes>                            |
    /// |                 v                          v                                  |
    /// |          metrics_port set                metrics_port set                     |
    /// |       <no> |          | <yes>         <yes> |         | <no>                  |
    /// |            v          v                     |         v                       |
    /// | nothing to do       create                  |       status == running         |
    /// |                                             |     <yes> |         | <no>      |
    /// |                                             |           v         v           |
    /// |                                             |    stop task      delete task   |
    /// |                                             v                                 |
    /// |                                  task_port == metrics_port                    |
    /// |                                 <yes> |              | <no>                   |
    /// |                                       v              v                        |
    /// |                           status == stopped       status == running           |
    /// |                          <yes> |              <yes> |         | <no>          |
    /// |                                v                    v         v               |
    /// |                             start task        stop task    delete task        |
    /// +-------------------------------------------------------------------------------+
    /// ```
    async fn process_monitoring(&self) -> NifiReconcileResult {
        let nifi_rest_endpoints = self
            .monitoring
            .list_nifi_rest_endpoints(self.existing_pods.as_slice())?;

        let metrics_port = self.context.resource.spec.metrics_port;

        let reporting_task = self
            .monitoring
            .find_reporting_task(
                &nifi_rest_endpoints,
                &self.context.resource.spec.version.to_string(),
            )
            .await?;

        if let Some(ReportingTask {
            revision,
            component,
            status: Some(ReportingTaskStatus { run_status, .. }),
            id,
            ..
        }) = reporting_task
        {
            let task_id = id.clone().unwrap_or_else(|| NO_TASK_ID.to_string());

            match (metrics_port, &run_status) {
                // If a metrics_port is set and the task is running, we need to check if the
                // metrics_port equals the NiFi ReportingTask metrics port.
                // We are done if they match, otherwise we need to stop the task
                (Some(port), ReportingTaskState::Running) => {
                    if !self
                        .monitoring
                        .match_metric_and_reporting_task_port(port, &component)
                    {
                        monitoring::try_with_nifi_rest_endpoints(
                            &nifi_rest_endpoints,
                            |endpoint| {
                                self.monitoring.update_reporting_task_status(
                                    endpoint,
                                    &task_id,
                                    &revision,
                                    ReportingTaskState::Stopped,
                                )
                            },
                        )
                        .await?;

                        info!("Stopped ReportingTask [{}]", task_id);

                        // requeue after stopping the task -> prepare for deletion
                        return Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(5)));
                    }
                }
                // If a metrics_port is set and the task is stopped, we need to check if the
                // metrics_port equals the NiFi ReportingTask metrics port.
                // If they match we need to start the task, if not we delete the task
                (Some(port), ReportingTaskState::Stopped) => {
                    return if self
                        .monitoring
                        .match_metric_and_reporting_task_port(port, &component)
                    {
                        monitoring::try_with_nifi_rest_endpoints(
                            &nifi_rest_endpoints,
                            |endpoint| {
                                self.monitoring.update_reporting_task_status(
                                    endpoint,
                                    &task_id,
                                    &revision,
                                    ReportingTaskState::Running,
                                )
                            },
                        )
                        .await?;

                        info!("Started ReportingTask [{}]", task_id);

                        // We can continue after we started a ReportingTask with the correct metrics port
                        Ok(ReconcileFunctionAction::Continue)
                    } else {
                        monitoring::try_with_nifi_rest_endpoints(
                            &nifi_rest_endpoints,
                            |endpoint| {
                                self.monitoring
                                    .delete_reporting_task(endpoint, &task_id, &revision)
                            },
                        )
                        .await?;

                        info!("Deleted ReportingTask [{}] - Different ports from metrics_port and reporting_task_port", task_id);

                        // requeue after deleting the task -> prepare for recreating
                        Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(5)))
                    };
                }
                // If no metrics port is set but a "Running" task is found, we need to stop it
                (None, ReportingTaskState::Running) => {
                    monitoring::try_with_nifi_rest_endpoints(&nifi_rest_endpoints, |endpoint| {
                        self.monitoring.update_reporting_task_status(
                            endpoint,
                            &task_id,
                            &revision,
                            ReportingTaskState::Stopped,
                        )
                    })
                    .await?;

                    info!("Stopped ReportingTask [{}]", task_id);

                    // requeue after stopping the task -> prepare for deletion
                    return Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(5)));
                }
                // If no metrics port is set but a "Stopped" task is found, we need to delete it
                (None, ReportingTaskState::Stopped) => {
                    monitoring::try_with_nifi_rest_endpoints(&nifi_rest_endpoints, |endpoint| {
                        self.monitoring
                            .delete_reporting_task(endpoint, &task_id, &revision)
                    })
                    .await?;

                    info!("Deleted ReportingTask [{}]", task_id);

                    return Ok(ReconcileFunctionAction::Continue);
                }
            }
        }
        // no reporting task available -> create it if metrics port available
        else if let Some(port) = metrics_port {
            let version = self.context.resource.spec.version.to_string();
            monitoring::try_with_nifi_rest_endpoints(&nifi_rest_endpoints, |endpoint| {
                self.monitoring
                    .create_reporting_task(endpoint, port, &version)
            })
            .await?;

            info!("Created ReportingTask");

            return Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(10)));
        }

        Ok(ReconcileFunctionAction::Continue)
    }
}

impl ReconciliationState for NifiState {
    type Error = error::NifiError;

    fn reconcile(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<ReconcileFunctionAction, Self::Error>> + Send + '_>>
    {
        info!("========================= Starting reconciliation =========================");

        Box::pin(async move {
            self.init_status()
                .await?
                .then(self.context.handle_deletion(
                    Box::pin(self.delete_all_pods()),
                    FINALIZER_NAME,
                    true,
                ))
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
                        .wait_for_running_and_ready_pods(self.existing_pods.as_slice()),
                )
                .await?
                .then(self.context.delete_excess_pods(
                    list_eligible_nodes_for_role_and_group(&self.eligible_nodes).as_slice(),
                    self.existing_pods.as_slice(),
                    ContinuationStrategy::OneRequeue,
                ))
                .await?
                .then(self.create_missing_pods())
                .await?
                .then(self.process_monitoring())
                .await
        })
    }
}

struct NifiStrategy {
    config: Arc<ProductConfigManager>,
    monitoring: Arc<NifiRestClient>,
}

impl NifiStrategy {
    pub fn new(config: ProductConfigManager, monitoring: NifiRestClient) -> NifiStrategy {
        NifiStrategy {
            config: Arc::new(config),
            monitoring: Arc::new(monitoring),
        }
    }
}

#[async_trait]
impl ControllerStrategy for NifiStrategy {
    type Item = NifiCluster;
    type State = NifiState;
    type Error = error::NifiError;

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
            monitoring: self.monitoring.clone(),
            existing_pods,
            eligible_nodes,
            zookeeper_info: None,
        })
    }
}

/// This creates an instance of a [`Controller`] which waits for incoming events and reconciles them.
///
/// This is an async method and the returned future needs to be consumed to make progress.
pub async fn create_controller(client: Client, product_config_path: &str) -> OperatorResult<()> {
    let nifi_api: Api<NifiCluster> = client.get_all_api();
    let pods_api: Api<Pod> = client.get_all_api();
    let configmaps_api: Api<ConfigMap> = client.get_all_api();

    let controller = Controller::new(nifi_api)
        .owns(pods_api, ListParams::default())
        .owns(configmaps_api, ListParams::default());

    let product_config = ProductConfigManager::from_yaml_file(product_config_path).unwrap();

    let monitoring = NifiRestClient::new(reqwest::Client::new());

    let strategy = NifiStrategy::new(product_config, monitoring);

    controller
        .run(client, strategy, Duration::from_secs(10))
        .await;

    Ok(())
}
