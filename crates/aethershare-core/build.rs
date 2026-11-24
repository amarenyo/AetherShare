use std::fs;
use std::path::Path;

fn main() {
    let version_file = Path::new("../../VERSION");
    println!("cargo:rerun-if-changed=../../VERSION");

    let version = fs::read_to_string(version_file)
        .ok()
        .map(|s| s.trim().trim_start_matches('v').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    println!("cargo:rustc-env=AETHERSHARE_VERSION={version}");
}
