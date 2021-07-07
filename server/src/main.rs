use stackable_nifi_crd::NifiCluster;
use stackable_operator::crd::Crd;
use stackable_operator::{client, error};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("NIFI_OPERATOR_LOG");

    info!("Starting Stackable Operator for Apache NiFi");

    let client = client::create_client(Some("nifi.stackable.tech".to_string())).await?;

    if let Err(error) = stackable_operator::crd::wait_until_crds_present(
        &client,
        vec![NifiCluster::RESOURCE_NAME],
        None,
    )
    .await
    {
        error!("Required CRDs missing, aborting: {:?}", error);
    };

    stackable_nifi_operator::create_controller(client).await;
    Ok(())
}
