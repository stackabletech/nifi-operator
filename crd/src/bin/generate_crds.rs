use stackable_nifi_crd::NifiCluster;
use stackable_operator::crd::CustomResourceExt;

fn main() {
    let target_file = "deploy/crd/nificluster.crd.yaml";
    NifiCluster::write_yaml_schema(target_file).unwrap();
    println!("Wrote CRD to [{}]", target_file);
}
