use stackable_nifi_crd::NifiCluster;
use stackable_operator::crd::CustomResourceExt;

fn main() -> Result<(), stackable_operator::error::Error> {
    built::write_built_file().expect("Failed to acquire build-time information");

    NifiCluster::write_yaml_schema("../deploy/crd/nificluster.crd.yaml")?;

    Ok(())
}
