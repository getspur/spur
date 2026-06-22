use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_NORMAL_DEPS: &[&str] = &[
    "spur-core",
    "spur-pm",
    "spur-graph",
    "spur-analyst",
    "spur-notebook",
    "spur-cost",
    "spur-license",
    "spur-blob-store",
    "spur-worktree",
];

const FORBIDDEN_SRC_PATTERNS: &[&str] = &[
    "use spur_core::",
    "spur_core::plan::",
    "spur_core::mcp::delegation",
    "spur_core::mcp::plan",
    "spur_core::worker_server",
    "DelegationRequest",
    "DelegationChannel",
    "BaseSpec",
    "BaseTarget",
    "OverlayCommit",
];

#[test]
fn spur_mcp_has_no_normal_domain_dependencies() {
    let manifest = fs::read_to_string(manifest_path()).expect("read spur-mcp Cargo.toml");
    let manifest: toml::Value = manifest.parse().expect("parse spur-mcp Cargo.toml");
    let deps = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("dependencies table");

    let actual: BTreeSet<&str> = FORBIDDEN_NORMAL_DEPS
        .iter()
        .copied()
        .filter(|name| deps.contains_key(*name))
        .collect();

    assert!(
        actual.is_empty(),
        "spur-mcp must stay below domain crates; remove normal dependencies: {actual:?}",
    );
}

#[test]
fn spur_mcp_source_does_not_import_core_domain_types() {
    let mut violations = Vec::new();
    for path in rust_sources(&crate_root().join("src")) {
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
        for pattern in FORBIDDEN_SRC_PATTERNS {
            if contents.contains(pattern) {
                violations.push(format!("{} contains `{pattern}`", rel(&path).display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "spur-mcp must not import or define core plan/reconciler/delegation domain types:\n{}",
        violations.join("\n"),
    );
}

fn manifest_path() -> PathBuf {
    crate_root().join("Cargo.toml")
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rust_sources(root, &mut out);
    out.sort();
    out
}

fn collect_rust_sources(path: &Path, out: &mut Vec<PathBuf>) {
    let metadata =
        fs::metadata(path).unwrap_or_else(|err| panic!("stat {}: {err}", path.display()));
    if metadata.is_file() {
        if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path.to_path_buf());
        }
        return;
    }

    let mut entries: Vec<PathBuf> = fs::read_dir(path)
        .unwrap_or_else(|err| panic!("read dir {}: {err}", path.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|err| panic!("read dir entry in {}: {err}", path.display()))
                .path()
        })
        .collect();
    entries.sort();
    for entry in entries {
        collect_rust_sources(&entry, out);
    }
}

fn rel(path: &Path) -> PathBuf {
    path.strip_prefix(crate_root())
        .unwrap_or(path)
        .to_path_buf()
}
