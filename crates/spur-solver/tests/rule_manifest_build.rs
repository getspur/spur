use std::{
    fs,
    path::{Path, PathBuf},
};

#[expect(dead_code)]
#[path = "../src/rules/manifest_format.rs"]
mod manifest_format;
#[path = "../build_support/manifest_source.rs"]
mod manifest_source;

use manifest_format::ManifestBundleV1;
use manifest_source::{
    canonical_manifest_json, load_manifest_sources, manifest_rerun_paths, write_canonical_manifest,
};

fn repository_manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/rules/families")
}

fn relative_paths(root: &Path, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .expect("manifest source must be below its root")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

fn copy_repository_sources(destination: &Path) -> PathBuf {
    let source_root = repository_manifest_root();
    let loaded = load_manifest_sources(&source_root).expect("repository manifests must load");
    let destination_root = destination.join("families");

    for source in loaded.source_paths {
        let relative = source
            .strip_prefix(&source_root)
            .expect("manifest source must be below its root");
        let target = destination_root.join(relative);
        fs::create_dir_all(target.parent().expect("manifest source parent"))
            .expect("create temporary manifest directory");
        fs::copy(&source, &target).expect("copy repository manifest source");
    }

    destination_root
}

fn replace_source(root: &Path, relative: &str, from: &str, to: &str) {
    let path = root.join(relative);
    let source = fs::read_to_string(&path).expect("read copied manifest source");
    let changed = source.replacen(from, to, 1);
    assert_ne!(changed, source, "replacement must modify {relative}");
    fs::write(path, changed).expect("write modified manifest source");
}

#[test]
fn approved_sources_load_in_deterministic_canonical_order() {
    let root = repository_manifest_root();
    let first = load_manifest_sources(&root).expect("repository manifests must load");
    let second = load_manifest_sources(&root).expect("repository manifests must load repeatedly");

    let expected_paths = [
        "accessibility/family.yaml",
        "accessibility/rules/focus_not_obscured.yaml",
        "accessibility/rules/reflow.yaml",
        "accessibility/rules/target_size.yaml",
        "accessibility/rules/text_contrast.yaml",
        "design/family.yaml",
        "design/rules/aspect_ratio.yaml",
        "design/rules/axis_capacity.yaml",
        "design/rules/containment.yaml",
        "design/rules/non_overlap.yaml",
        "policy/family.yaml",
        "policy/rules/dynamic_separation_of_duty.yaml",
        "policy/rules/minimum_privilege.yaml",
        "policy/rules/permission_reachable.yaml",
        "policy/rules/role_hierarchy_acyclic.yaml",
        "policy/rules/static_separation_of_duty.yaml",
        "resource/family.yaml",
        "resource/rules/aggregate_capacity.yaml",
        "resource/rules/minimum_failure_domains.yaml",
        "resource/rules/quota_capacity.yaml",
        "resource/rules/request_within_limit.yaml",
        "resource/rules/topology_max_skew.yaml",
    ];
    assert_eq!(relative_paths(&root, &first.source_paths), expected_paths);
    assert_eq!(first, second);

    let family_ids = first
        .bundle
        .families
        .iter()
        .map(|family| family.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        family_ids,
        ["accessibility", "design", "policy", "resource"]
    );
    assert!(first
        .bundle
        .rules
        .windows(2)
        .all(|rules| rules[0].id < rules[1].id));

    let json = canonical_manifest_json(&first.bundle).expect("serialize canonical bundle");
    assert_eq!(json, canonical_manifest_json(&second.bundle).unwrap());
    assert!(!json.contains('\n'), "canonical JSON must be compact");
    let round_trip: ManifestBundleV1 =
        serde_json::from_str(&json).expect("canonical JSON must deserialize");
    assert_eq!(round_trip, first.bundle);
}

#[test]
fn discovery_ignores_yaml_outside_approved_source_paths() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    let expected = load_manifest_sources(&root).expect("copied manifests must load");

    fs::write(root.join("accessibility/notes.yaml"), "not: a manifest\n")
        .expect("write unapproved family-side YAML");
    fs::write(
        root.join("accessibility/rules/notes.yml"),
        "not: a manifest\n",
    )
    .expect("write unapproved extension");
    fs::create_dir_all(root.join("accessibility/rules/nested"))
        .expect("create unapproved nested rules directory");
    fs::write(
        root.join("accessibility/rules/nested/notes.yaml"),
        "not: a manifest\n",
    )
    .expect("write unapproved nested YAML");
    fs::create_dir_all(root.join("unapproved/rules")).expect("create unapproved family directory");
    fs::write(root.join("unapproved/family.yaml"), "not: a manifest\n")
        .expect("write unapproved family YAML");

    let actual = load_manifest_sources(&root).expect("unapproved YAML must be ignored");
    assert_eq!(actual, expected);
}

#[test]
fn strict_parse_failures_include_source_and_rule_context() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    let relative = "accessibility/rules/target_size.yaml";
    let path = root.join(relative);
    let source = fs::read_to_string(&path).expect("read copied rule");
    fs::write(&path, format!("{source}unknown_field: true\n")).expect("malform copied rule");

    let error = load_manifest_sources(&root).expect_err("unknown fields must fail strictly");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(relative), "{diagnostic}");
    assert!(diagnostic.contains("rule `target_size`"), "{diagnostic}");
    assert!(diagnostic.contains("unknown field"), "{diagnostic}");
}

#[test]
fn cross_document_failures_include_source_and_rule_id() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    let relative = "accessibility/rules/target_size.yaml";
    let path = root.join(relative);
    let source = fs::read_to_string(&path).expect("read copied rule");
    fs::write(
        &path,
        source.replacen("family: \"accessibility\"", "family: \"missing\"", 1),
    )
    .expect("break copied rule ownership");

    let error = load_manifest_sources(&root).expect_err("unknown family must fail");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(relative), "{diagnostic}");
    assert!(
        diagnostic.contains("rule `a11y.target_size`"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("unknown family `missing`"),
        "{diagnostic}"
    );
}

#[test]
fn implemented_handlers_must_form_a_bijection_with_native_handler_all() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    fs::remove_file(root.join("accessibility/rules/focus_not_obscured.yaml"))
        .expect("remove one implemented manifest");

    let error = load_manifest_sources(&root).expect_err("missing native handler must fail");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("manifest bundle"), "{diagnostic}");
    assert!(
        diagnostic.contains("a11y_focus_not_obscured"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("missing"), "{diagnostic}");
}

#[test]
fn native_handlers_must_belong_to_the_declared_rule_family() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "accessibility/rules/focus_not_obscured.yaml",
        "handler: \"a11y_focus_not_obscured\"",
        "handler: \"rbac_role_hierarchy_acyclic\"",
    );
    replace_source(
        &root,
        "policy/rules/role_hierarchy_acyclic.yaml",
        "handler: rbac_role_hierarchy_acyclic",
        "handler: a11y_focus_not_obscured",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("cross-family native handlers must fail")
        .to_string();
    assert!(diagnostic.contains("handler"), "{diagnostic}");
    assert!(diagnostic.contains("family"), "{diagnostic}");
}

#[test]
fn native_handler_defaulted_parameters_must_keep_a_default() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "resource/rules/topology_max_skew.yaml",
        "    default: 1\n",
        "",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("native handler default drift must fail")
        .to_string();
    assert!(diagnostic.contains("max_skew"), "{diagnostic}");
    assert!(diagnostic.contains("default"), "{diagnostic}");
}

#[test]
fn native_handler_required_parameters_must_remain_required() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "design/rules/axis_capacity.yaml",
        "    required: true\n    kind: \"string_enum\"",
        "    required: false\n    kind: \"string_enum\"",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("native handler required-parameter drift must fail")
        .to_string();
    assert!(diagnostic.contains("axis"), "{diagnostic}");
    assert!(diagnostic.contains("required"), "{diagnostic}");
}

#[test]
fn conformance_diagnostics_must_match_the_rule_violation_diagnostic() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "resource/rules/request_within_limit.yaml",
        "      expected_diagnostic: resource.request_within_limit.violation",
        "      expected_diagnostic: wrong.violation",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("conformance diagnostic drift must fail")
        .to_string();
    assert!(diagnostic.contains("expected_diagnostic"), "{diagnostic}");
    assert!(diagnostic.contains("wrong.violation"), "{diagnostic}");
}

#[test]
fn canonical_output_and_rerun_paths_cover_every_source() {
    let root = repository_manifest_root();
    let loaded = load_manifest_sources(&root).expect("repository manifests must load");
    let rerun_paths = manifest_rerun_paths(&root, &loaded.source_paths);
    for source in &loaded.source_paths {
        assert!(
            rerun_paths.contains(source),
            "missing rerun path: {}",
            source.display()
        );
    }
    for family in ["accessibility", "design", "policy", "resource"] {
        assert!(rerun_paths.contains(&root.join(family)));
        assert!(rerun_paths.contains(&root.join(family).join("rules")));
    }

    let temp = tempfile::tempdir().expect("temporary output directory");
    let output = temp.path().join("spur_rule_manifests_v1.json");
    write_canonical_manifest(&output, &loaded.bundle).expect("write canonical manifest bundle");
    assert_eq!(
        fs::read_to_string(output).expect("read canonical manifest bundle"),
        canonical_manifest_json(&loaded.bundle).expect("serialize canonical manifest bundle")
    );
}
