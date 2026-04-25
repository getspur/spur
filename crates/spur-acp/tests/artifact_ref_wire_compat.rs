//! Golden round-trip test: `ArtifactRef` wire format must remain stable
//! across releases. The `#[serde(flatten)]` attribute on `kind` is a
//! wire-shape invariant — removing it changes the on-the-wire JSON from
//! `{"kind":"patch",...}` to `{"kind":{"kind":"patch"},...}`.
//!
//! This test reads a frozen golden fixture, deserializes into the current
//! `ArtifactRef` shape, re-serializes, and asserts that the structural
//! `kind` projection still produces the flat shape.

use serde_json::{json, Value};
use spur_acp::domain::ArtifactRef;

fn load_fixture() -> Value {
    let raw = include_str!("data/artifact_ref_v0.json");
    serde_json::from_str(raw).expect("fixture must parse")
}

#[test]
fn patch_unit_variant_round_trips_with_flat_kind() {
    let fixture = load_fixture();
    let payload = fixture
        .get("patch_unit")
        .cloned()
        .expect("patch_unit entry");

    let parsed: ArtifactRef = serde_json::from_value(payload.clone())
        .expect("patch_unit must deserialize into current ArtifactRef shape");

    let reserialized = serde_json::to_value(&parsed).expect("re-serialize");

    assert_eq!(
        reserialized.get("kind"),
        Some(&Value::String("patch".into())),
        "kind must remain a flat string on the wire (#[serde(flatten)] preserved)"
    );
    assert_eq!(reserialized.get("uri"), payload.get("uri"));
    assert_eq!(reserialized.get("byte_size"), payload.get("byte_size"));
    assert_eq!(reserialized.get("sha256"), payload.get("sha256"));
}

#[test]
fn other_named_variant_round_trips_with_flat_name_projection() {
    let fixture = load_fixture();
    let payload = fixture
        .get("other_named")
        .cloned()
        .expect("other_named entry");

    let parsed: ArtifactRef = serde_json::from_value(payload.clone())
        .expect("other_named must deserialize into current ArtifactRef shape");

    let reserialized = serde_json::to_value(&parsed).expect("re-serialize");

    assert_eq!(
        reserialized.get("kind"),
        Some(&Value::String("other".into())),
        "kind discriminator stays flat for data-carrying variants"
    );
    assert_eq!(
        reserialized.get("name"),
        Some(&Value::String("worker_artifact".into())),
        "Other variant's String payload surfaces as a sibling \"name\" field"
    );
}

#[test]
fn old_payloads_without_new_fields_deserialize_with_none() {
    let fixture = load_fixture();
    let payload = fixture.get("patch_unit").cloned().unwrap();

    let parsed: ArtifactRef = serde_json::from_value(payload).expect("deserialize");

    assert!(parsed.git_object_ref.is_none());
    assert!(parsed.git_blob_sha.is_none());
}

#[test]
fn new_payloads_with_optional_fields_omit_them_when_none() {
    let fresh = ArtifactRef {
        kind: spur_acp::domain::continuation::ArtifactKind::Patch,
        uri: "spur://artifact/x".into(),
        byte_size: 0,
        sha256: None,
        git_object_ref: None,
        git_blob_sha: None,
    };

    let serialized = serde_json::to_value(&fresh).unwrap();
    assert!(serialized.get("git_object_ref").is_none(), "None field must be omitted");
    assert!(serialized.get("git_blob_sha").is_none());
    assert!(serialized.get("sha256").is_none());

    let with_meta = ArtifactRef {
        git_object_ref: Some("refs/spur/artifacts/sess-1".into()),
        git_blob_sha: Some("a".repeat(40)),
        ..fresh
    };
    let serialized2 = serde_json::to_value(&with_meta).unwrap();
    assert_eq!(
        serialized2.get("git_object_ref"),
        Some(&json!("refs/spur/artifacts/sess-1"))
    );
    assert_eq!(serialized2.get("git_blob_sha"), Some(&json!("a".repeat(40))));
}
