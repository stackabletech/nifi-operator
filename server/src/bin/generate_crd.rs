use stackable_nifi_crd::NiFiCluster;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("{}", serde_yaml::to_string(&NiFiCluster::crd())?);
    Ok(())
}
