use stackable_nifi_crd::NifiCluster;
use stackable_operator::{client, error};

#[tokio::main]
async fn main() -> Result<(), error::Error> {
    stackable_operator::logging::initialize_logging("NIFI_OPERATOR_LOG");

    let client = client::create_client(Some("nifi.stackable.tech".to_string())).await?;
    stackable_nifi_operator::create_controller(client).await;
    Ok(())
}
