use std::{env, path::PathBuf};

#[allow(dead_code)]
#[path = "src/rules/manifest_format.rs"]
mod manifest_format;
#[path = "build_support/manifest_source.rs"]
mod manifest_source;

fn main() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest_root = crate_root.join("src/rules/families");
    let loaded = manifest_source::load_manifest_sources(&manifest_root).unwrap_or_else(|error| {
        panic!("failed to build declarative solver rule manifest bundle: {error}")
    });

    println!(
        "cargo:rerun-if-changed={}",
        crate_root.join("src/rules/manifest_format.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        crate_root
            .join("build_support/manifest_source.rs")
            .display()
    );
    for path in manifest_source::manifest_rerun_paths(&manifest_root, &loaded.source_paths) {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"))
        .join("spur_rule_manifests_v1.json");
    manifest_source::write_canonical_manifest(&output, &loaded.bundle).unwrap_or_else(|error| {
        panic!("failed to build declarative solver rule manifest bundle: {error}")
    });
}
