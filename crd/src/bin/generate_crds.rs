use kube::CustomResourceExt;
use stackable_nifi_crd::NifiCluster;
use std::error::Error;
use std::fs;

fn main() -> Result<(), Box<dyn Error>> {
    write_crd::<NifiCluster>("deploy/crd/nificluster.crd.yaml");
    Ok(())
}

fn write_crd<T: CustomResourceExt>(file_path: &str) {
    let schema = T::crd();
    let string_schema = match serde_yaml::to_string(&schema) {
        Ok(schema) => schema,
        Err(err) => panic!("Failed to retrieve CRD: [{}]", err),
    };
    match fs::write(file_path, string_schema) {
        Ok(()) => println!("Successfully wrote CRD to file [{}].", file_path),
        Err(err) => println!("Failed to write file: [{}]", err),
    }
}
