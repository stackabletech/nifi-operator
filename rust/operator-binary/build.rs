use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is required"));
    built::write_built_file_with_opts(Some("Cargo.toml".as_ref()), &out_dir.join("built.rs"))
        .unwrap();
}
