use stackable_nifi_crd::NifiCluster;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("{}", serde_yaml::to_string(&NifiCluster::crd())?);
    Ok(())
}
