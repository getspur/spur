use std::fs;
use std::path::Path;

use spur_graph::extract::languages::{all_supported_extensions, Language};
use spur_graph::{build_facts, NodeKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

fn extract_labels(files: &[(&str, &str)]) -> Vec<(String, NodeKind)> {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, source) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create fixture dirs");
        }
        fs::write(&full, source).expect("write fixture");
    }
    let facts = build_facts(dir.path(), None).expect("extract").0;
    facts
        .nodes
        .into_iter()
        .filter(|node| node.kind != NodeKind::File)
        .map(|node| (node.label, node.kind))
        .collect()
}

#[test]
fn json_toml_yaml_paths_route_to_structured_languages() {
    assert_eq!(
        Language::from_path(Path::new("package.json")),
        Some(Language::Json)
    );
    assert_eq!(
        Language::from_path(Path::new("Cargo.toml")),
        Some(Language::Toml)
    );
    assert_eq!(
        Language::from_path(Path::new(".github/workflows/ci.yaml")),
        Some(Language::Yaml)
    );
    assert_eq!(
        Language::from_path(Path::new("chart.yml")),
        Some(Language::Yaml)
    );
}

#[test]
fn jupyter_keeps_ipynb_and_does_not_share_json() {
    assert_eq!(
        Language::from_path(Path::new("notebook.ipynb")),
        Some(Language::JupyterNotebook)
    );
    assert_ne!(
        Language::from_path(Path::new("config.json")),
        Language::from_path(Path::new("notebook.ipynb"))
    );
}

#[test]
fn lockfile_basenames_are_not_structured_languages() {
    for path in [
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "pnpm-lock.yml",
        "composer.lock",
        "poetry.lock",
        "uv.lock",
        "yarn.lock",
        "bun.lock",
        "Gemfile.lock",
        "flake.lock",
    ] {
        assert_eq!(
            Language::from_path(Path::new(path)),
            None,
            "{path} must stay out of structured extraction"
        );
    }
}

#[test]
fn supported_extensions_include_json_toml_yaml() {
    let extensions: Vec<_> = all_supported_extensions();
    for extension in ["json", "toml", "yaml", "yml"] {
        assert!(
            extensions.iter().any(|candidate| *candidate == extension),
            "missing extension {extension}"
        );
    }
}

#[test]
fn json_tags_query_captures_named_keys() {
    let language: tree_sitter::Language = tree_sitter_json::LANGUAGE.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("configure json parser");
    let source = r#"{
  "name": "spur",
  "dependencies": {
    "left-pad": "1.0.0"
  }
}"#;
    let tree = parser.parse(source, None).expect("parse json");
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let query =
        Query::new(&language, include_str!("../queries/json/tags.scm")).expect("compile json tags");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut found = Vec::new();
    while let Some(query_match) = matches.next() {
        for capture in query_match.captures {
            found.push((
                capture_names[capture.index as usize].to_string(),
                capture
                    .node
                    .utf8_text(source.as_bytes())
                    .expect("text")
                    .to_string(),
            ));
        }
    }
    assert!(
        found
            .iter()
            .any(|(name, text)| name == "definition.constant" || text.contains("name")),
        "sexp={} captures={found:?}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn json_named_keys_become_module_field_and_constant() {
    let labels = extract_labels(&[(
        "package.json",
        r#"{
  "name": "spur",
  "private": true,
  "dependencies": {
    "left-pad": "1.0.0"
  },
  "keywords": ["cli"]
}"#,
    )]);
    assert!(
        labels.contains(&("name".to_string(), NodeKind::Constant)),
        "top-level scalar name should be Constant, got {labels:?}"
    );
    assert!(
        labels.contains(&("dependencies".to_string(), NodeKind::Module)),
        "object key dependencies should be Module, got {labels:?}"
    );
    assert!(
        labels.contains(&("left-pad".to_string(), NodeKind::Field)),
        "nested scalar left-pad should be Field, got {labels:?}"
    );
    assert!(
        !labels.iter().any(|(label, _)| label == "keywords"),
        "array keys must be skipped, got {labels:?}"
    );
}

#[test]
fn toml_named_tables_and_root_scalars_extract() {
    let labels = extract_labels(&[(
        "Cargo.toml",
        r#"
name = "workspace-root"

[package]
name = "spur-graph"
version = "1.0.0"

[dependencies]
serde = "1"
"#,
    )]);
    assert!(
        labels.contains(&("name".to_string(), NodeKind::Constant)),
        "root scalar name should be Constant, got {labels:?}"
    );
    assert!(
        labels.contains(&("package".to_string(), NodeKind::Module)),
        "[package] should be Module, got {labels:?}"
    );
    assert!(
        labels.contains(&("version".to_string(), NodeKind::Field)),
        "package.version should be Field, got {labels:?}"
    );
    assert!(
        labels.contains(&("dependencies".to_string(), NodeKind::Module)),
        "[dependencies] should be Module, got {labels:?}"
    );
}

#[test]
fn yaml_named_mappings_extract_and_sequences_are_skipped() {
    let labels = extract_labels(&[(
        "ci.yaml",
        r#"
name: ci
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
"#,
    )]);
    assert!(
        labels.contains(&("name".to_string(), NodeKind::Constant)),
        "top-level scalar name should be Constant, got {labels:?}"
    );
    assert!(
        labels.contains(&("jobs".to_string(), NodeKind::Module)),
        "jobs mapping should be Module, got {labels:?}"
    );
    assert!(
        labels.contains(&("build".to_string(), NodeKind::Module)),
        "job name build should be Module, got {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|(label, _)| label == "steps" || label == "uses"),
        "sequence items must be skipped, got {labels:?}"
    );
}

#[test]
fn package_lock_json_emits_no_symbols() {
    let labels = extract_labels(&[(
        "package-lock.json",
        r#"{ "name": "should-not-index", "lockfileVersion": 3 }"#,
    )]);
    assert!(
        labels.is_empty(),
        "lockfiles must not produce symbols, got {labels:?}"
    );
}
