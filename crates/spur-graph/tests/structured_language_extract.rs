use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use spur_graph::extract::languages::{all_supported_extensions, Language};
use spur_graph::{build_facts, NodeKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

/// Same wall-clock budget as graph rebuild / overlay wait
/// (`DEFAULT_GRAPH_REBUILD_LATENCY_BUDGET`; solve_id sol_15aa7cd790284b20).
const JSON_EXTRACT_LATENCY_BUDGET: Duration = Duration::from_millis(750);

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
fn mermaid_paths_route_to_mermaid_language() {
    assert_eq!(
        Language::from_path(Path::new("design.mmd")),
        Some(Language::Mermaid)
    );
    assert_eq!(
        Language::from_path(Path::new("flow.mermaid")),
        Some(Language::Mermaid)
    );
}

#[test]
fn mermaid_flowchart_emits_vertex_modules() {
    let labels = extract_labels(&[(
        "flow.mmd",
        "flowchart TD\n    SPEC[spec]\n    CHECK[check]\n    SPEC --> CHECK\n",
    )]);
    assert!(
        labels
            .iter()
            .any(|(label, kind)| label == "SPEC" && *kind == NodeKind::Module),
        "expected Module SPEC, got {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|(label, kind)| label == "CHECK" && *kind == NodeKind::Module),
        "expected Module CHECK, got {labels:?}"
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

fn elevenlabs_openapi_source() -> String {
    let sibling = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../spur-notebook/rest-table-gateway/specs/tier-a/elevenlabs.json");
    fs::read_to_string(sibling).unwrap_or_else(|_| synthetic_openapi_spec())
}

fn synthetic_openapi_spec() -> String {
    let mut paths = String::new();
    for i in 0..18 {
        paths.push_str(&format!(
            r##"    "/v1/item-{i}": {{
      "get": {{
        "operationId": "get_item_{i}",
        "description": "Returns metadata about generated audio item {i}.",
        "parameters": [
          {{
            "name": "xi-api-key",
            "in": "header",
            "schema": {{ "type": "string", "title": "Xi-Api-Key" }}
          }}
        ],
        "responses": {{
          "200": {{
            "description": "Successful Response",
            "content": {{
              "application/json": {{
                "schema": {{ "$ref": "#/components/schemas/Item{i}" }}
              }}
            }}
          }}
        }}
      }}
    }}{comma}
"##,
            comma = if i + 1 < 18 { "," } else { "" }
        ));
    }
    format!(
        r#"{{
  "openapi": "3.0.2",
  "info": {{ "title": "ElevenLabs API Documentation", "version": "1.0" }},
  "paths": {{
{paths}  }},
  "components": {{
    "schemas": {{
      "Item0": {{ "type": "object", "properties": {{ "id": {{ "type": "string" }} }} }}
    }}
  }}
}}"#
    )
}

#[test]
fn json_extract_of_elevenlabs_openapi_completes_within_rebuild_budget() {
    let source = elevenlabs_openapi_source();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let labels = extract_labels(&[("specs/tier-a/elevenlabs.json", &source)]);
        let _ = tx.send(labels);
    });
    let labels = rx
        .recv_timeout(JSON_EXTRACT_LATENCY_BUDGET)
        .unwrap_or_else(|_| {
            panic!(
                "JSON OpenAPI extract exceeded rebuild budget {:?}",
                JSON_EXTRACT_LATENCY_BUDGET
            )
        });
    assert!(
        labels.iter().any(|(label, kind)| {
            label == "get" && matches!(kind, NodeKind::Module | NodeKind::Field)
        }),
        "expected HTTP get object key, got {labels:?}"
    );
}

/// Hub names Option B keeps: path template, HTTP method, operationId *value*,
/// and `components.schemas` name. Nested `properties` keys are the hang vector
/// (`data_integrity.cardinality` sol_60192356cb874f86 / sol_c46a8dd2968f4645).
fn small_openapi_json() -> &'static str {
    r#"{
  "openapi": "3.0.2",
  "info": { "title": "Demo", "version": "1.0" },
  "paths": {
    "/v1/pets": {
      "get": {
        "operationId": "listPets",
        "parameters": [
          { "name": "limit", "in": "query", "schema": { "type": "integer" } }
        ]
      }
    }
  },
  "components": {
    "schemas": {
      "Pet": {
        "type": "object",
        "properties": {
          "nested_prop_id": { "type": "string" }
        }
      }
    }
  }
}"#
}

fn small_openapi_yaml() -> &'static str {
    r#"
openapi: "3.0.2"
info:
  title: Demo
  version: "1.0"
paths:
  /v1/pets:
    get:
      operationId: listPets
      parameters:
        - name: limit
          in: query
components:
  schemas:
    Pet:
      type: object
      properties:
        nested_prop_id:
          type: string
"#
}

/// Property-heavy OpenAPI: enough nested schema fields that naive named-key
/// extract + O(n²) `nearest_parent` exceeds `JSON_EXTRACT_LATENCY_BUDGET`
/// (github.json scale was 208942 keys / ~53 min; solve_id sol_f2da2a57a44b4c4e).
fn property_heavy_openapi_json() -> String {
    const PATHS: usize = 24;
    const SCHEMAS: usize = 24;
    const PROPERTIES: usize = 200;
    let mut paths = String::new();
    for i in 0..PATHS {
        let comma = if i + 1 < PATHS { "," } else { "" };
        paths.push_str(&format!(
            r##"    "/v1/item-{i}": {{
      "get": {{
        "operationId": "get_item_{i}",
        "parameters": [
          {{ "name": "xi-api-key", "in": "header", "schema": {{ "type": "string" }} }}
        ]
      }}
    }}{comma}
"##
        ));
    }
    let mut schemas = String::new();
    for schema in 0..SCHEMAS {
        let mut properties = String::new();
        for prop in 0..PROPERTIES {
            let comma = if prop + 1 < PROPERTIES { "," } else { "" };
            properties.push_str(&format!(
                r#"        "nested_prop_{schema}_{prop}": {{ "type": "string" }}{comma}
"#
            ));
        }
        let comma = if schema + 1 < SCHEMAS { "," } else { "" };
        schemas.push_str(&format!(
            r#"      "Item{schema}": {{
        "type": "object",
        "properties": {{
{properties}        }}
      }}{comma}
"#
        ));
    }
    format!(
        r#"{{
  "openapi": "3.0.2",
  "info": {{ "title": "Heavy", "version": "1.0" }},
  "paths": {{
{paths}  }},
  "components": {{
    "schemas": {{
{schemas}    }}
  }}
}}"#
    )
}

fn assert_openapi_hubs(labels: &[(String, NodeKind)]) {
    assert!(
        labels
            .iter()
            .any(|(label, _)| label == "/v1/pets" || label == "/v1/item-0"),
        "expected path template, got {labels:?}"
    );
    assert!(
        labels.iter().any(|(label, kind)| {
            label == "get" && matches!(kind, NodeKind::Module | NodeKind::Field)
        }),
        "expected HTTP get, got {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|(label, _)| label == "listPets" || label == "get_item_0"),
        "expected operationId value, got {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|(label, kind)| (label == "Pet" || label == "Item0") && *kind == NodeKind::Module),
        "expected schema name Module, got {labels:?}"
    );
}

#[test]
fn json_openapi_extract_emits_operation_id_path_method_and_schema_names() {
    let labels = extract_labels(&[("openapi.json", small_openapi_json())]);
    assert_openapi_hubs(&labels);
}

#[test]
fn json_openapi_extract_skips_nested_schema_property_keys() {
    let labels = extract_labels(&[("openapi.json", small_openapi_json())]);
    assert!(
        !labels.iter().any(|(label, _)| label == "nested_prop_id"),
        "nested schema properties must not be extracted, got {labels:?}"
    );
    assert!(
        !labels.iter().any(|(label, _)| label == "limit"),
        "parameter names must not be extracted, got {labels:?}"
    );
}

#[test]
fn json_openapi_extract_of_property_heavy_spec_completes_within_rebuild_budget() {
    let source = property_heavy_openapi_json();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let labels = extract_labels(&[("specs/tier-a/heavy.json", &source)]);
        let _ = tx.send(labels);
    });
    let labels = rx
        .recv_timeout(JSON_EXTRACT_LATENCY_BUDGET)
        .unwrap_or_else(|_| {
            panic!(
                "property-heavy OpenAPI extract exceeded rebuild budget {:?}",
                JSON_EXTRACT_LATENCY_BUDGET
            )
        });
    assert!(
        labels.iter().any(|(label, _)| label == "get_item_0"),
        "expected operationId get_item_0, got {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|(label, kind)| label == "Item0" && *kind == NodeKind::Module),
        "expected schema Item0, got {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|(label, _)| label.starts_with("nested_prop_")),
        "nested schema properties must not be extracted, got {} labels",
        labels.len()
    );
}

#[test]
fn yaml_openapi_extract_emits_hubs_and_skips_nested_properties() {
    let labels = extract_labels(&[("openapi.yaml", small_openapi_yaml())]);
    assert_openapi_hubs(&labels);
    assert!(
        !labels.iter().any(|(label, _)| label == "nested_prop_id"),
        "YAML nested schema properties must not be extracted, got {labels:?}"
    );
}

/// Max generic JSON/YAML bytes whose naive named-key extract still fits the
/// 750ms rebuild budget at elevenlabs key density (1228 keys / 62_000 bytes /
/// 110ms). Witness: max_keys=3206 `sol_30c3db6ac2074bb1`; 3207 unsat
/// `sol_9d016ca3d4fa4223`; max_bytes=161866 `sol_623e3687e68546e4`.
const MAX_STRUCTURED_EXTRACT_BYTES: usize = 161_866;

fn generic_heavy_json_dump() -> String {
    let mut body = String::from("{\n  \"dump\": {\n");
    let mut index = 0usize;
    while body.len() <= MAX_STRUCTURED_EXTRACT_BYTES {
        if index > 0 {
            body.push_str(",\n");
        }
        body.push_str(&format!(
            "    \"nested_prop_{index}\": {{ \"type\": \"string\" }}"
        ));
        index += 1;
    }
    body.push_str("\n  }\n}\n");
    body
}

fn generic_heavy_yaml_dump() -> String {
    let mut body = String::from("dump:\n");
    let mut index = 0usize;
    while body.len() <= MAX_STRUCTURED_EXTRACT_BYTES {
        body.push_str(&format!("  nested_prop_{index}: value\n"));
        index += 1;
    }
    body
}

fn extract_labels_within_rebuild_budget(path: &str, source: String) -> Vec<(String, NodeKind)> {
    let path = path.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let labels = extract_labels(&[(&path, &source)]);
        let _ = tx.send(labels);
    });
    rx.recv_timeout(JSON_EXTRACT_LATENCY_BUDGET)
        .unwrap_or_else(|_| {
            panic!(
                "generic structured extract exceeded rebuild budget {:?}",
                JSON_EXTRACT_LATENCY_BUDGET
            )
        })
}

#[test]
fn json_generic_extract_over_byte_cap_skips_named_keys_within_rebuild_budget() {
    let source = generic_heavy_json_dump();
    assert!(
        source.len() > MAX_STRUCTURED_EXTRACT_BYTES,
        "fixture must exceed byte cap, got {}",
        source.len()
    );
    assert!(
        !source.contains("openapi") && !source.contains("swagger"),
        "fixture must stay generic JSON, not OpenAPI"
    );
    let labels = extract_labels_within_rebuild_budget("dump.json", source);
    assert!(
        !labels
            .iter()
            .any(|(label, _)| label.starts_with("nested_prop_")),
        "generic JSON over byte cap must skip named keys, got {} labels",
        labels.len()
    );
}

#[test]
fn yaml_generic_extract_over_byte_cap_skips_named_keys_within_rebuild_budget() {
    let source = generic_heavy_yaml_dump();
    assert!(
        source.len() > MAX_STRUCTURED_EXTRACT_BYTES,
        "fixture must exceed byte cap, got {}",
        source.len()
    );
    let labels = extract_labels_within_rebuild_budget("dump.yaml", source);
    assert!(
        !labels
            .iter()
            .any(|(label, _)| label.starts_with("nested_prop_")),
        "generic YAML over byte cap must skip named keys, got {} labels",
        labels.len()
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
