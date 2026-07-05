use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Value};
use spur_analyst::mcp::{AnalystMcpModule, McpHandlerError};

const INIT_SEARCH_SQL: &str = include_str!("../../spur-context/analyst/init_search.sql");
const EMBED_MODE_ENV: &str = "SPUR_ANALYST_EMBED_MODE";
const EXPECTED_TOOL_NAMES: &[&str] = &[
    "doc_navigate",
    "knowledge_context_pack",
    "knowledge_context_pack_2",
    "query",
];
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct AnalystFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db_path: PathBuf,
}

impl AnalystFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join(".spur")).expect("create .spur");
        let db_path = root.join(".spur").join("analyst.duckdb");
        seed_analyst_db(&db_path);
        Self {
            _dir: dir,
            root,
            db_path,
        }
    }
}

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self {
            _lock: lock,
            key,
            previous,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn analyst_mcp_module_advertises_exact_public_tool_names() {
    let module = AnalystMcpModule::new();
    let names = module
        .tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();

    assert_eq!(names, EXPECTED_TOOL_NAMES);
}

#[test]
fn doc_navigate_is_split_into_application_modules_and_thin_mcp_adapter() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let doc_nav = src.join("doc_nav");

    for module in ["mod.rs", "artifact.rs", "query.rs", "projection.rs"] {
        let path = doc_nav.join(module);
        assert!(path.is_file(), "missing doc_nav module {}", path.display());
        assert!(
            line_count(&path) < 300,
            "{} should stay below 300 lines",
            path.display()
        );
    }

    let adapter = src.join("mcp").join("tools").join("doc_navigate.rs");
    assert!(
        adapter.is_file(),
        "missing thin MCP adapter {}",
        adapter.display()
    );
    assert!(
        line_count(&adapter) <= 80,
        "{} should remain a thin adapter",
        adapter.display()
    );

    let old_module = src.join("mcp").join("doc_navigate.rs");
    assert!(
        !old_module.exists(),
        "{} should move into doc_nav/* plus mcp/tools/doc_navigate.rs",
        old_module.display()
    );
}

#[test]
fn query_is_split_into_thin_tool_adapter_and_arrow_value_module() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let adapter = src.join("mcp").join("tools").join("query.rs");
    let arrow_values = src.join("mcp").join("value").join("arrow.rs");

    for path in [&adapter, &arrow_values] {
        assert!(
            path.is_file(),
            "missing query split module {}",
            path.display()
        );
        assert!(
            line_count(path) < 260,
            "{} should stay below 260 lines",
            path.display()
        );
    }

    let old_module = src.join("mcp").join("query.rs");
    assert!(
        !old_module.exists(),
        "{} should move into mcp/tools/query.rs plus mcp/value/arrow.rs",
        old_module.display()
    );
}

#[tokio::test]
async fn analyst_mcp_dispatch_keeps_all_public_tool_names_reachable() {
    let module = AnalystMcpModule::new();

    for tool_name in EXPECTED_TOOL_NAMES {
        let error = module
            .dispatch(tool_name, json!({}))
            .await
            .expect_err("empty args should fail inside the routed tool");
        assert!(
            !is_unknown_tool_error(&error),
            "{tool_name} should route to its handler, got {error:?}"
        );
    }

    let unknown = module
        .dispatch("__missing_tool__", json!({}))
        .await
        .expect_err("unknown tool should fail");
    assert!(is_unknown_tool_error(&unknown));
}

#[tokio::test]
async fn knowledge_context_pack_v1_response_shape_matches_snapshot() {
    let _embed_mode = EnvGuard::set(EMBED_MODE_ENV, "off");
    let fixture = AnalystFixture::new();
    let pack = dispatch_in_fixture(
        &fixture,
        "knowledge_context_pack",
        json!({
            "query": "dispatch approval evidence",
            "intent": "review",
            "scope": "all",
            "limit": 5,
            "max_symbol_bodies": 0
        }),
    )
    .await
    .expect("v1 fixture response");

    assert!(pack.get("error").is_none(), "{pack:#}");
    assert_json_snapshot(
        "knowledge_context_pack_v1_shape",
        &normalize_pack_snapshot(pack, &fixture.db_path),
    );
}

#[tokio::test]
async fn knowledge_context_pack_v2_response_shape_matches_snapshot() {
    let _embed_mode = EnvGuard::set(EMBED_MODE_ENV, "off");
    let fixture = AnalystFixture::new();
    let pack = dispatch_in_fixture(
        &fixture,
        "knowledge_context_pack_2",
        json!({
            "query": "dispatch approval evidence",
            "intent": "review",
            "scope": "all",
            "limit": 5,
            "max_symbol_bodies": 0,
            "graph_reasoning": {
                "paths": true,
                "communities": true,
                "risk": true,
                "max_path_hops": 2,
                "max_paths": 1
            }
        }),
    )
    .await
    .expect("v2 fixture response");

    assert!(pack.get("error").is_none(), "{pack:#}");
    assert_json_snapshot(
        "knowledge_context_pack_v2_shape",
        &normalize_pack_snapshot(pack, &fixture.db_path),
    );
}

async fn dispatch_in_fixture(
    fixture: &AnalystFixture,
    tool_name: &'static str,
    args: Value,
) -> Result<Value, McpHandlerError> {
    let module = AnalystMcpModule::new();
    let root = fixture.root.clone();
    spur_graph::mcp::with_worktree_root_for_request(root, async move {
        module.dispatch(tool_name, args).await
    })
    .await
}

fn is_unknown_tool_error(error: &McpHandlerError) -> bool {
    matches!(error, McpHandlerError::InvalidParams(message) if message.contains("unknown analyst MCP tool"))
}

fn assert_json_snapshot(name: &str, actual: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.json"));
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(actual).expect("snapshot JSON should serialize")
    );
    let expected = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "failed to read snapshot {}: {error}\nactual snapshot:\n{actual}",
            path.display()
        );
    });
    assert_eq!(expected, actual, "snapshot mismatch for {}", path.display());
}

fn line_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        .lines()
        .count()
}

fn normalize_pack_snapshot(mut value: Value, db_path: &Path) -> Value {
    replace_string_value(
        &mut value,
        &db_path.display().to_string(),
        "<fixture-analyst-db>",
    );
    value
}

fn replace_string_value(value: &mut Value, needle: &str, replacement: &str) {
    match value {
        Value::String(string) if string == needle => {
            *string = replacement.to_owned();
        }
        Value::Array(values) => {
            for value in values {
                replace_string_value(value, needle, replacement);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                replace_string_value(value, needle, replacement);
            }
        }
        _ => {}
    }
}

fn seed_analyst_db(db_path: &Path) {
    let conn = duckdb::Connection::open(db_path).expect("open fixture db");
    conn.execute_batch("INSTALL fts; LOAD fts; INSTALL icu; LOAD icu; INSTALL lance; LOAD lance;")
        .expect("load fixture extensions");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('kcp-fixture-hash');

        CREATE TABLE sections_search (
            stable_symbol_id VARCHAR,
            qualified_name VARCHAR,
            file_path VARCHAR,
            heading_level INTEGER,
            content_hash VARCHAR,
            body_text VARCHAR
        );
        INSERT INTO sections_search VALUES
            ('doc-dispatch', 'Dispatch Approval Reading Path', 'docs/dispatch.md', 2, 'doc-hash',
             'dispatch approval evidence reading path');

        CREATE TABLE symbol_text (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            file_path VARCHAR,
            symbol_kind VARCHAR,
            doc_text VARCHAR
        );
        INSERT INTO symbol_text VALUES
            ('sym-dispatch', 'dispatch_plan', 'fixture::dispatch_plan',
             'src/dispatch.rs', 'function', 'dispatch approval evidence entry point'),
            ('sym-review', 'review_approval', 'fixture::review_approval',
             'src/review.rs', 'function', 'dispatch approval evidence review path');

        CREATE TABLE v_symbol_scorecard (
            stable_symbol_id VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR,
            file_path VARCHAR,
            pagerank DOUBLE,
            in_degree BIGINT,
            out_degree BIGINT,
            callers BIGINT,
            importers BIGINT,
            inbound_total BIGINT,
            churn_90d BIGINT,
            last_touched TIMESTAMP,
            blast_radius_score DOUBLE,
            posture VARCHAR
        );
        INSERT INTO v_symbol_scorecard VALUES
            ('sym-dispatch', 'dispatch_plan', 'fixture::dispatch_plan', 'function', 'src/dispatch.rs',
             0.42, 7, 3, 11, 2, 13, 9, TIMESTAMP '2026-06-17 12:00:00', 0.91, 'load-bearing wall'),
            ('sym-review', 'review_approval', 'fixture::review_approval', 'function', 'src/review.rs',
             0.21, 2, 1, 3, 0, 3, 1, TIMESTAMP '2026-06-16 09:30:00', 0.33, 'stable');

        CREATE TABLE v_symbol_inbound (
            stable_symbol_id VARCHAR,
            callers BIGINT
        );
        INSERT INTO v_symbol_inbound VALUES
            ('sym-dispatch', 11),
            ('sym-review', 3);

        CREATE TABLE nodes (
            stable_symbol_id VARCHAR,
            node_id BIGINT,
            file_path VARCHAR,
            entity_name VARCHAR,
            qualified_name VARCHAR,
            symbol_kind VARCHAR
        );
        INSERT INTO nodes VALUES
            ('sym-dispatch', 1, 'src/dispatch.rs', 'dispatch_plan', 'fixture::dispatch_plan', 'function'),
            ('sym-review', 2, 'src/review.rs', 'review_approval', 'fixture::review_approval', 'function');

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            src_id BIGINT,
            dst_id BIGINT,
            target_label VARCHAR,
            relation VARCHAR,
            confidence VARCHAR,
            confidence_score FLOAT,
            edge_kind VARCHAR,
            bind_method VARCHAR
        );
        INSERT INTO edges VALUES
            ('sym-dispatch', 'sym-review', 1, 2, 'review_approval', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton');

        CREATE TABLE v_symbol_component (
            stable_symbol_id VARCHAR,
            component_id BIGINT,
            component_size BIGINT
        );
        INSERT INTO v_symbol_component VALUES
            ('sym-dispatch', 10, 2),
            ('sym-review', 10, 2);

        CREATE TABLE v_symbol_community (
            stable_symbol_id VARCHAR,
            community_id BIGINT
        );
        INSERT INTO v_symbol_community VALUES
            ('sym-dispatch', 20),
            ('sym-review', 20);

        CREATE TABLE v_graph_metrics (
            calls_edges BIGINT,
            connected_nodes BIGINT,
            components BIGINT,
            largest_component BIGINT,
            communities BIGINT,
            density DOUBLE
        );
        INSERT INTO v_graph_metrics VALUES (1, 2, 1, 2, 1, 0.5);
        "#,
    )
    .expect("create fixture schema");
    conn.execute_batch(
        r#"
        PRAGMA create_fts_index('main.sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');
        PRAGMA create_fts_index('main.symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);
        "#,
    )
    .expect("create fixture fts indexes");
    conn.execute_batch(&context_candidate_macro_sql())
        .expect("define context candidate macro");
}

fn context_candidate_macro_sql() -> String {
    INIT_SEARCH_SQL
        .split("CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE")
        .nth(1)
        .and_then(|rest| rest.split("-- Graph-augmented:").next())
        .map(|body| {
            let start =
                "CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE";
            format!("{start}{body}")
        })
        .expect("context candidate macro should be present in init_search.sql")
}
