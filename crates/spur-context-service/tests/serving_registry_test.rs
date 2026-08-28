use spur_context_service::serving_registry::{
    ArtifactRef, ServingPackage, ServingRegistry, SERVING_REGISTRY_SCHEMA_VERSION,
};

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn artifact(uri: &str, sha256: &str, bytes: u64) -> ArtifactRef {
    ArtifactRef {
        uri: uri.to_owned(),
        sha256: sha256.to_owned(),
        bytes,
    }
}

fn package(source: &str, package: &str, revision: &str, generation: i64) -> ServingPackage {
    let artifact_prefix = format!("s3://spur-artifacts/{generation}/{package}/{revision}");
    ServingPackage {
        source: source.to_owned(),
        package: package.to_owned(),
        revision: revision.to_owned(),
        revision_kind: "git_sha".to_owned(),
        refs: Vec::new(),
        generation,
        graph_prefix_uri: format!("{artifact_prefix}/graph/"),
        graph_manifest: artifact(
            &format!("{artifact_prefix}/graph/manifest.json"),
            SHA_A,
            128,
        ),
        source_sidecar: artifact(
            &format!("{artifact_prefix}/source/source_files.parquet"),
            SHA_B,
            256,
        ),
    }
}

fn complete_registry(generation: i64) -> ServingRegistry {
    ServingRegistry {
        schema_version: SERVING_REGISTRY_SCHEMA_VERSION,
        generation,
        packages: vec![
            package("github", "acme/widget", "rev-a", generation),
            package("gitlab", "acme/gadget", "rev-b", generation),
        ],
    }
}

#[test]
fn registry_rejects_package_from_another_generation() {
    let mut registry = complete_registry(7);
    registry.packages[0].generation = 6;

    assert_eq!(
        registry.validate().unwrap_err().code(),
        "generation_mismatch"
    );
}

#[test]
fn registry_rejects_duplicate_package_identity() {
    let mut registry = complete_registry(7);
    registry.packages.push(registry.packages[0].clone());

    assert_eq!(
        registry.validate().unwrap_err().code(),
        "duplicate_package_identity"
    );
}

#[test]
fn registry_rejects_empty_package_identity_fields() {
    let mut empty_source = complete_registry(7);
    empty_source.packages[0].source = "  ".to_owned();
    let mut empty_package = complete_registry(7);
    empty_package.packages[0].package.clear();
    let mut empty_revision = complete_registry(7);
    empty_revision.packages[0].revision.clear();

    for registry in [empty_source, empty_package, empty_revision] {
        assert_eq!(
            registry.validate().unwrap_err().code(),
            "empty_package_field"
        );
    }
}

#[test]
fn registry_rejects_non_s3_artifact_uris() {
    let mut graph_prefix = complete_registry(7);
    graph_prefix.packages[0].graph_prefix_uri = "https://example.com/graph/".to_owned();
    let mut graph_manifest = complete_registry(7);
    graph_manifest.packages[0].graph_manifest.uri = "file:///tmp/manifest.json".to_owned();
    let mut source_sidecar = complete_registry(7);
    source_sidecar.packages[0].source_sidecar.uri = "source_files.parquet".to_owned();

    for registry in [graph_prefix, graph_manifest, source_sidecar] {
        assert_eq!(
            registry.validate().unwrap_err().code(),
            "invalid_artifact_uri"
        );
    }
}

#[test]
fn registry_rejects_missing_artifact_references() {
    let mut missing_uri = complete_registry(7);
    missing_uri.packages[0].graph_manifest.uri.clear();
    let mut missing_hash = complete_registry(7);
    missing_hash.packages[0].source_sidecar.sha256.clear();

    for registry in [missing_uri, missing_hash] {
        assert_eq!(
            registry.validate().unwrap_err().code(),
            "missing_artifact_ref"
        );
    }
}

#[test]
fn registry_deserialization_requires_both_artifact_references() {
    let mut encoded = serde_json::to_value(complete_registry(7)).unwrap();
    encoded["packages"][0]
        .as_object_mut()
        .unwrap()
        .remove("graph_manifest");

    assert!(serde_json::from_value::<ServingRegistry>(encoded).is_err());
}

#[test]
fn registry_rejects_unsupported_schema_version() {
    let mut registry = complete_registry(7);
    registry.schema_version += 1;

    assert_eq!(
        registry.validate().unwrap_err().code(),
        "unsupported_schema_version"
    );
}

#[test]
fn registry_rejects_malformed_sha256_values() {
    for malformed in ["a".repeat(63), "g".repeat(64), "é".repeat(64)] {
        let mut registry = complete_registry(7);
        registry.packages[0].graph_manifest.sha256 = malformed;

        assert_eq!(registry.validate().unwrap_err().code(), "invalid_sha256");
    }
}

#[test]
fn registry_rejects_zero_byte_artifacts() {
    let mut empty_graph = complete_registry(7);
    empty_graph.packages[0].graph_manifest.bytes = 0;
    let mut empty_source = complete_registry(7);
    empty_source.packages[0].source_sidecar.bytes = 0;

    for registry in [empty_graph, empty_source] {
        assert_eq!(
            registry.validate().unwrap_err().code(),
            "zero_byte_artifact"
        );
    }
}

#[test]
fn canonical_serialization_is_versioned_and_package_order_independent() {
    let registry = complete_registry(7);
    let mut reversed = registry.clone();
    reversed.packages.reverse();

    let canonical = registry.to_canonical_json().unwrap();
    assert_eq!(canonical, reversed.to_canonical_json().unwrap());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&canonical).unwrap()["schema_version"],
        SERVING_REGISTRY_SCHEMA_VERSION
    );
}

#[test]
fn resolution_requires_an_exact_identity_in_a_valid_registry() {
    let registry = complete_registry(7);

    assert_eq!(
        registry
            .resolve("github", "acme/widget", "rev-a")
            .unwrap()
            .unwrap()
            .revision,
        "rev-a"
    );
    assert!(registry
        .resolve("github", "acme/widget", "rev-b")
        .unwrap()
        .is_none());
}

#[test]
fn resolution_never_falls_back_across_a_mixed_generation() {
    let mut registry = complete_registry(7);
    registry.packages[0].generation = 6;

    assert_eq!(
        registry
            .resolve("gitlab", "acme/gadget", "rev-b")
            .unwrap_err()
            .code(),
        "generation_mismatch"
    );
}

#[test]
fn resolution_supports_named_refs_without_guessing_revision_order() {
    let mut registry = complete_registry(7);
    registry.packages[0].refs = vec!["latest".to_owned(), "stable".to_owned()];

    for reference in ["latest", "stable"] {
        assert_eq!(
            registry
                .resolve_revision_or_ref("github", "acme/widget", reference)
                .unwrap()
                .unwrap()
                .revision,
            "rev-a"
        );
    }
}

#[test]
fn registry_rejects_one_ref_pointing_at_multiple_revisions() {
    let mut registry = complete_registry(7);
    registry.packages[0].refs = vec!["latest".to_owned()];
    registry.packages[1].source = registry.packages[0].source.clone();
    registry.packages[1].package = registry.packages[0].package.clone();
    registry.packages[1].refs = vec!["latest".to_owned()];

    assert_eq!(
        registry.validate().unwrap_err().code(),
        "duplicate_package_ref"
    );
}

#[test]
fn registry_loader_upgrades_v1_without_a_serving_outage() {
    let generation = 7;
    let mut prerelease = package(
        "registry:crates-io",
        "acme/widget",
        "1.0.0-alpha.1",
        generation,
    );
    prerelease.revision_kind = "semver".to_owned();
    let mut stable = package("registry:crates-io", "acme/widget", "1.0.0", generation);
    stable.revision_kind = "semver".to_owned();
    stable.graph_manifest.sha256 = SHA_B.to_owned();

    let mut encoded = serde_json::json!({
        "schema_version": 1,
        "generation": generation,
        "packages": [prerelease, stable],
    });
    for package in encoded["packages"].as_array_mut().unwrap() {
        package.as_object_mut().unwrap().remove("revision_kind");
        package.as_object_mut().unwrap().remove("refs");
    }

    let bytes = serde_json::to_vec(&encoded).unwrap();
    let registry = ServingRegistry::from_json_slice(&bytes).unwrap();

    assert_eq!(registry.schema_version, SERVING_REGISTRY_SCHEMA_VERSION);
    assert_eq!(
        registry
            .resolve_revision_or_ref("registry:crates-io", "acme/widget", "latest")
            .unwrap()
            .unwrap()
            .revision,
        "1.0.0"
    );
}
