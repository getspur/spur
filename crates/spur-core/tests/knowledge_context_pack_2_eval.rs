use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use duckdb::{params, Connection};
use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_core::server::{community_feature_gate, DetachedContinuationCtx};
use spur_core::McpCallbackServer;
use spur_graph::{
    artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer,
    GraphIndexArtifact, WriteOptions,
};
use tempfile::TempDir;

static CWD_LOCK: Mutex<()> = Mutex::new(());

const QUERY_CONNECTED: &str = "connected subsystem path";
const QUERY_RISK: &str = "ambiguous sink risk";
const QUERY_WEAK: &str = "weak retrieval";
const QUERY_MEDIUM: &str = "medium retrieval";
const QUERY_STRONG: &str = "strong retrieval";
const QUERY_DISJOINT: &str = "disjoint singletons";
const QUERY_SCOPE: &str = "scope sentinel";

struct CwdGuard {
    original: PathBuf,
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

struct EvalFixture {
    _temp_dir: TempDir,
    root: PathBuf,
    ids: HashMap<&'static str, String>,
    graph_hash: String,
}

fn enter_dir(path: &Path) -> CwdGuard {
    let original = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(path).expect("set current dir");
    CwdGuard { original }
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn test_server() -> McpCallbackServer {
    let session_id = BrainSessionId::new(SessionId("brain-kcp2-eval".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        community_feature_gate(),
    );
    server
}

fn write_fixture_crate(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("create src dir");
    std::fs::create_dir_all(root.join("docs")).expect("create docs dir");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"kcp2-eval-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write manifest");

    let mut source = String::from(
        r#"
pub trait EvalStep {
    fn dyn_execute(&self);
}

pub struct EvalWorker;

impl EvalStep for EvalWorker {
    fn dyn_execute(&self) {
        target_leaf();
    }
}

pub fn root_entry(step: &dyn EvalStep) {
    dyn_bridge(step);
}

pub fn dyn_bridge(step: &dyn EvalStep) {
    step.dyn_execute();
}

pub fn target_leaf() {}

pub fn common_sink() {}

pub fn control_leaf() {}

pub fn control_caller() {
    control_leaf();
}

pub fn isolated_one() {}

pub fn isolated_two() {}

pub fn weak_helper() {}

pub fn medium_helper() {}

pub fn strong_helper_a() {}

pub fn strong_helper_b() {}

pub fn strong_helper_c() {}

"#,
    );
    for index in 0..31 {
        source.push_str(&format!(
            "pub fn sink_caller_{index:02}() {{\n    common_sink();\n}}\n\n"
        ));
    }
    std::fs::write(root.join("src/lib.rs"), source).expect("write fixture source");
    std::fs::write(
        root.join("docs/scope.md"),
        "# Scope Sentinel\n\nscope sentinel documentation row.\n",
    )
    .expect("write fixture doc");
}

fn write_graph_artifact(root: &Path, artifact: &GraphIndexArtifact) {
    let artifact_base = root.join(".spur/graph");
    let written = write_artifact_parquet(
        artifact,
        &artifact_base,
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    write_current_pointer(root, &written).expect("write current pointer");
}

fn build_graph_artifact(root: &Path) -> GraphIndexArtifact {
    let (facts, _file_counts) = build_facts(root, None).expect("build graph facts");
    let artifact = artifact_from_facts(&facts, root).expect("build graph artifact");
    write_graph_artifact(root, &artifact);
    artifact
}

fn symbol_ids(artifact: &GraphIndexArtifact) -> HashMap<&'static str, String> {
    let wanted = [
        "root_entry",
        "dyn_bridge",
        "target_leaf",
        "common_sink",
        "control_leaf",
        "isolated_one",
        "isolated_two",
        "weak_helper",
        "medium_helper",
        "strong_helper_a",
        "strong_helper_b",
        "strong_helper_c",
    ];
    wanted
        .into_iter()
        .map(|entity_name| {
            let symbol = artifact
                .symbols
                .iter()
                .find(|symbol| {
                    symbol.entity_name == entity_name && symbol.symbol_kind == "function"
                })
                .unwrap_or_else(|| panic!("fixture symbol exists: {entity_name}"));
            (entity_name, symbol.stable_symbol_id.clone())
        })
        .collect()
}

fn id<'a>(ids: &'a HashMap<&'static str, String>, entity_name: &'static str) -> &'a str {
    ids.get(entity_name)
        .unwrap_or_else(|| panic!("stable id exists for {entity_name}"))
}

#[allow(clippy::too_many_arguments)]
fn insert_context_row(
    conn: &Connection,
    row_order: i64,
    query_key: &str,
    kind: &str,
    title: &str,
    file_path: &str,
    stable_symbol_id: &str,
    symbol_kind: &str,
    score: f64,
    signal: Option<&str>,
    neighbor_kind: Option<&str>,
    grounding: &str,
) {
    conn.execute(
        "INSERT INTO context_rows VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            row_order,
            query_key,
            kind,
            title,
            file_path,
            stable_symbol_id,
            symbol_kind,
            score,
            signal,
            neighbor_kind,
            Option::<&str>::None,
            grounding,
            false
        ],
    )
    .expect("insert context row");
}

fn insert_scorecard(
    conn: &Connection,
    stable_symbol_id: &str,
    entity_name: &str,
    callers: i64,
    churn_90d: i64,
    posture: &str,
) {
    conn.execute(
        "INSERT INTO v_symbol_scorecard VALUES (?, ?, ?, 'function', ?, ?, ?, ?, ?, 0, ?, ?, ?, ?, ?)",
        params![
            stable_symbol_id,
            entity_name,
            format!("fixture::{entity_name}"),
            "src/lib.rs",
            if posture == "leaf" { 0.001 } else { 0.05 },
            callers,
            1_i64,
            callers,
            callers,
            churn_90d,
            "2026-06-19 00:00:00",
            if callers > 30 { 0.90 } else { 0.20 },
            posture
        ],
    )
    .expect("insert scorecard row");
    conn.execute(
        "INSERT INTO v_symbol_inbound VALUES (?, ?)",
        params![stable_symbol_id, callers],
    )
    .expect("insert inbound row");
    conn.execute(
        "INSERT INTO v_symbol_component VALUES (?, ?, ?)",
        params![stable_symbol_id, 10_i64, 8_i64],
    )
    .expect("insert component row");
    conn.execute(
        "INSERT INTO v_symbol_community VALUES (?, ?)",
        params![stable_symbol_id, 20_i64],
    )
    .expect("insert community row");
}

fn add_code_row(
    conn: &Connection,
    ids: &HashMap<&'static str, String>,
    row_order: &mut i64,
    query_key: &str,
    entity_name: &'static str,
    score: f64,
    signal: &str,
    grounding: &str,
) {
    *row_order += 1;
    insert_context_row(
        conn,
        *row_order,
        query_key,
        "code",
        entity_name,
        "src/lib.rs",
        id(ids, entity_name),
        "function",
        score,
        Some(signal),
        Some("primary"),
        grounding,
    );
}

fn add_doc_row(conn: &Connection, row_order: &mut i64, query_key: &str, title: &str, score: f64) {
    *row_order += 1;
    insert_context_row(
        conn,
        *row_order,
        query_key,
        "doc",
        title,
        "docs/scope.md",
        "doc-scope-sentinel",
        "section",
        score,
        None,
        None,
        "bm25-doc",
    );
}

fn create_analyst_db(root: &Path, ids: &HashMap<&'static str, String>, graph_hash: &str) {
    std::fs::create_dir_all(root.join(".spur")).expect("create .spur dir");
    let db_path = root.join(".spur/analyst.duckdb");
    let conn = Connection::open(&db_path).expect("open analyst fixture db");
    conn.execute_batch(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);

        CREATE TABLE context_rows (
            row_order BIGINT,
            query_key VARCHAR,
            kind VARCHAR,
            title VARCHAR,
            file_path VARCHAR,
            stable_symbol_id VARCHAR,
            symbol_kind VARCHAR,
            score DOUBLE,
            signal VARCHAR,
            neighbor_kind VARCHAR,
            edge_bind_method VARCHAR,
            grounding VARCHAR,
            from_graph BOOLEAN
        );

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

        CREATE TABLE v_symbol_inbound (
            stable_symbol_id VARCHAR,
            callers BIGINT
        );

        CREATE TABLE v_symbol_component (
            stable_symbol_id VARCHAR,
            component_id BIGINT,
            component_size BIGINT
        );

        CREATE TABLE v_symbol_community (
            stable_symbol_id VARCHAR,
            community_id BIGINT
        );

        CREATE TABLE v_graph_metrics (
            calls_edges BIGINT,
            connected_nodes BIGINT,
            components BIGINT,
            largest_component BIGINT,
            communities BIGINT,
            density DOUBLE
        );

        CREATE TABLE edges (
            source_stable_id VARCHAR,
            target_stable_id VARCHAR,
            relation VARCHAR,
            edge_kind VARCHAR,
            confidence VARCHAR,
            bind_method VARCHAR
        );

        CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE
          SELECT kind, title, file_path, stable_symbol_id, symbol_kind,
                 round(
                   score * CASE
                     WHEN intent = 'review' AND signal LIKE '%load-bearing wall%' THEN 1.35
                     WHEN intent = 'change' AND kind IN ('code', 'symbol') THEN 1.10
                     ELSE 1.0
                   END,
                   3
                 ) AS score,
                 signal, neighbor_kind, edge_bind_method, grounding
          FROM context_rows
          WHERE query_key = q
            AND from_graph = false
            AND (
              requested_scope = 'all'
              OR (requested_scope = 'docs' AND kind = 'doc')
              OR (requested_scope IN ('code', 'graph') AND kind IN ('code', 'symbol'))
            )
          ORDER BY score DESC, row_order ASC
          LIMIT 40;

        CREATE OR REPLACE MACRO search_graph(q, intent) AS TABLE
          SELECT kind, title, file_path, stable_symbol_id, symbol_kind,
                 round(score, 3) AS score,
                 signal, neighbor_kind, edge_bind_method, grounding
          FROM context_rows
          WHERE query_key = q
            AND kind IN ('code', 'symbol')
          ORDER BY row_order ASC
          LIMIT 20;
        "#,
    )
    .expect("create analyst fixture schema");
    conn.execute("INSERT INTO _meta VALUES (?)", params![graph_hash])
        .expect("insert graph hash");
    conn.execute(
        "INSERT INTO v_graph_metrics VALUES (8, 8, 2, 5, 3, 0.22)",
        [],
    )
    .expect("insert graph metrics");

    let scorecard_rows = [
        ("root_entry", 0, 9, "active"),
        ("dyn_bridge", 1, 7, "active"),
        ("target_leaf", 1, 3, "stable"),
        ("common_sink", 0, 0, "leaf"),
        ("control_leaf", 1, 1, "stable"),
        ("isolated_one", 0, 0, "leaf"),
        ("isolated_two", 0, 0, "leaf"),
        ("weak_helper", 0, 0, "leaf"),
        ("medium_helper", 0, 2, "stable"),
        ("strong_helper_a", 0, 4, "stable"),
        ("strong_helper_b", 0, 3, "stable"),
        ("strong_helper_c", 0, 2, "stable"),
    ];
    for (entity_name, callers, churn, posture) in scorecard_rows {
        insert_scorecard(
            &conn,
            id(ids, entity_name),
            entity_name,
            callers,
            churn,
            posture,
        );
    }

    let mut row_order = 0_i64;

    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_CONNECTED,
        "root_entry",
        12.0,
        "active - pr=500.0 - churn=9",
        "bm25-code",
    );
    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_CONNECTED,
        "target_leaf",
        7.0,
        "stable - pr=500.0 - churn=3",
        "bm25-code",
    );

    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_RISK,
        "common_sink",
        12.0,
        "leaf - pr=10.0 - churn=0",
        "bm25-code",
    );
    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_RISK,
        "control_leaf",
        7.0,
        "stable - pr=500.0 - churn=1",
        "bm25-code",
    );

    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_WEAK,
        "weak_helper",
        2.0,
        "leaf - pr=10.0 - churn=0",
        "bm25-code",
    );

    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_MEDIUM,
        "medium_helper",
        5.0,
        "stable - pr=500.0 - churn=2",
        "bm25-code",
    );
    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_MEDIUM,
        "control_leaf",
        4.0,
        "stable - pr=500.0 - churn=1",
        "bm25-code",
    );

    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_STRONG,
        "strong_helper_a",
        12.0,
        "stable - pr=500.0 - churn=4",
        "bm25-code",
    );
    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_STRONG,
        "strong_helper_b",
        4.0,
        "stable - pr=500.0 - churn=3",
        "bm25-code",
    );
    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_STRONG,
        "strong_helper_c",
        3.0,
        "stable - pr=500.0 - churn=2",
        "bm25-code",
    );
    add_doc_row(
        &conn,
        &mut row_order,
        QUERY_STRONG,
        "Strong Retrieval Note",
        2.0,
    );

    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_DISJOINT,
        "isolated_one",
        9.0,
        "leaf - pr=10.0 - churn=0",
        "bm25-code",
    );
    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_DISJOINT,
        "isolated_two",
        8.0,
        "leaf - pr=10.0 - churn=0",
        "bm25-code",
    );

    add_code_row(
        &conn,
        ids,
        &mut row_order,
        QUERY_SCOPE,
        "root_entry",
        10.0,
        "active - pr=500.0 - churn=9",
        "bm25-code",
    );
    add_doc_row(
        &conn,
        &mut row_order,
        QUERY_SCOPE,
        "Scope Sentinel Docs",
        9.0,
    );

    conn.execute(
        "INSERT INTO edges VALUES (?, ?, 'calls', 'calls_dyn', 'dynamic_dispatch', 'label_match')",
        params![id(ids, "root_entry"), id(ids, "dyn_bridge")],
    )
    .expect("insert calls_dyn edge");
    conn.execute(
        "INSERT INTO edges VALUES (?, ?, 'calls', 'calls_dyn', 'dynamic_dispatch', 'label_match')",
        params![id(ids, "root_entry"), id(ids, "dyn_bridge")],
    )
    .expect("insert duplicate calls_dyn edge");
    conn.execute(
        "INSERT INTO edges VALUES (?, ?, 'calls', 'calls', 'syntax_exact', 'singleton')",
        params![id(ids, "dyn_bridge"), id(ids, "target_leaf")],
    )
    .expect("insert calls edge");
    conn.execute(
        "INSERT INTO edges VALUES (?, ?, 'calls', 'calls', 'syntax_exact', 'singleton')",
        params![id(ids, "dyn_bridge"), id(ids, "target_leaf")],
    )
    .expect("insert duplicate calls edge");
    conn.execute(
        "INSERT INTO edges VALUES (?, ?, 'contains', 'references_other', 'syntax_exact', 'scope')",
        params![id(ids, "root_entry"), id(ids, "weak_helper")],
    )
    .expect("insert containment noise");
    conn.execute(
        "INSERT INTO edges VALUES (?, ?, 'imports', 'references_other', 'syntax_exact', 'external')",
        params![id(ids, "weak_helper"), id(ids, "target_leaf")],
    )
    .expect("insert import noise");
}

fn build_eval_fixture(stale_graph_hash: Option<&str>) -> EvalFixture {
    let temp_dir = TempDir::new().expect("tempdir");
    let root = temp_dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("create repo root");
    write_fixture_crate(&root);
    let artifact = build_graph_artifact(&root);
    let ids = symbol_ids(&artifact);
    let analyst_hash = stale_graph_hash.unwrap_or(&artifact.graph_content_hash);
    create_analyst_db(&root, &ids, analyst_hash);
    EvalFixture {
        _temp_dir: temp_dir,
        root,
        ids,
        graph_hash: artifact.graph_content_hash,
    }
}

async fn call_pack(root: &Path, args: Value) -> Value {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let _cwd = enter_dir(root);
    let server = test_server();
    let response = server
        .__test_call_tool("knowledge_context_pack_2", args)
        .await;
    assert!(
        response.get("error").is_none(),
        "tool should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("successful tool response with text content: {response}"));
    serde_json::from_str(text).expect("tool text is JSON")
}

fn primary_titles(pack: &Value) -> Vec<&str> {
    pack["primary_evidence"]
        .as_array()
        .expect("primary evidence")
        .iter()
        .filter_map(|row| row["title"].as_str())
        .collect()
}

fn caveats_with_code<'a>(pack: &'a Value, code: &str) -> Vec<&'a Value> {
    pack["caveats"]
        .as_array()
        .expect("caveats")
        .iter()
        .filter(|caveat| caveat["code"] == code)
        .collect()
}

fn risk_row<'a>(pack: &'a Value, stable_symbol_id: &str) -> &'a Value {
    pack["risk_scorecard"]
        .as_array()
        .expect("risk scorecard")
        .iter()
        .find(|row| row["stable_symbol_id"] == stable_symbol_id)
        .unwrap_or_else(|| panic!("risk row exists for {stable_symbol_id}: {pack:#}"))
}

fn graph_path_sequences(pack: &Value) -> Vec<Vec<String>> {
    let mut rows_by_path: BTreeMap<i64, Vec<&Value>> = BTreeMap::new();
    for row in pack["graph_paths"][0]["rows"]
        .as_array()
        .expect("path rows")
    {
        rows_by_path
            .entry(row["path_index"].as_i64().expect("path index"))
            .or_default()
            .push(row);
    }

    rows_by_path
        .values()
        .map(|rows| {
            let mut sequence = rows
                .iter()
                .map(|row| {
                    row["source_stable_id"]
                        .as_str()
                        .expect("source id")
                        .to_string()
                })
                .collect::<Vec<_>>();
            sequence.push(
                rows.last().expect("non-empty path")["target_stable_id"]
                    .as_str()
                    .expect("target id")
                    .to_string(),
            );
            sequence
        })
        .collect()
}

#[tokio::test]
#[ignore = "TODO(phase4): port main kcp2 graph-path refinements into spur-analyst handler"]
async fn connected_subsystem_paths_are_calls_dyn_inclusive_and_deduped() {
    let fixture = build_eval_fixture(None);

    let pack = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_CONNECTED,
            "intent": "review",
            "scope": "code",
            "limit": 5,
            "graph_reasoning": {
                "paths": true,
                "communities": true,
                "risk": false,
                "max_path_hops": 3,
                "max_paths": 4
            }
        }),
    )
    .await;

    assert_eq!(pack["graph_content_hash"], fixture.graph_hash);
    assert_eq!(pack["staleness"]["analyst_matches_exact_graph"], true);
    assert_eq!(pack["graph_paths"][0]["status"], "path_found");
    assert_eq!(pack["graph_paths"][0]["traversal"], "undirected");
    let rows = pack["graph_paths"][0]["rows"]
        .as_array()
        .expect("path rows");
    assert!(
        rows.iter().all(|row| row["relation"] == "calls"
            && matches!(row["edge_kind"].as_str(), Some("calls" | "calls_dyn"))),
        "path rows must be calls-only and exclude containment/import noise: {rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row["edge_kind"] == "calls_dyn"),
        "calls_dyn hop should be retained: {rows:#?}"
    );
    assert_eq!(
        graph_path_sequences(&pack),
        vec![vec![
            id(&fixture.ids, "root_entry").to_string(),
            id(&fixture.ids, "dyn_bridge").to_string(),
            id(&fixture.ids, "target_leaf").to_string(),
        ]],
        "duplicate full node sequences should collapse before max_paths"
    );

    let community = pack["community_context"][0]
        .as_object()
        .expect("community row");
    assert!(community.contains_key("community_id"));
    assert!(!community.contains_key("component_id"));
    assert!(!community.contains_key("component_size"));
}

#[tokio::test]
#[ignore = "TODO(phase4): port main kcp2 graph-path refinements into spur-analyst handler"]
async fn ambiguous_sink_risk_reconciles_exact_inbound_and_bounds_popular_sink() {
    let fixture = build_eval_fixture(None);

    let pack = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_RISK,
            "intent": "review",
            "scope": "code",
            "limit": 5,
            "graph_reasoning": {
                "paths": false,
                "communities": true,
                "risk": true
            }
        }),
    )
    .await;

    assert_eq!(primary_titles(&pack)[0], "common_sink");
    assert_eq!(
        pack["primary_evidence"][0]["impact"]["popular_sink"], true,
        "ambiguous bare-name sink should be counted as popular: {pack:#}"
    );
    assert_eq!(
        pack["impact"]["summary"],
        "popular sink counted but not expanded"
    );
    assert_eq!(
        pack["impact"]["caller_neighbors"]
            .as_array()
            .expect("caller neighbors")
            .len(),
        0,
        "popular sink should bound expansion"
    );

    let sink = risk_row(&pack, id(&fixture.ids, "common_sink"));
    assert_eq!(sink["posture"], "leaf");
    assert_eq!(sink["callers"], 0);
    assert_eq!(sink["label_inbound"], 31);
    assert_eq!(sink["inbound_unresolved"], 31);
    assert_eq!(sink["name_ambiguous"], true);

    let control = risk_row(&pack, id(&fixture.ids, "control_leaf"));
    assert_eq!(control["label_inbound"], 1);
    assert_eq!(control["inbound_unresolved"], 0);
    assert_eq!(control["name_ambiguous"], false);
}

#[tokio::test]
async fn confidence_calibration_spans_low_medium_high() {
    let fixture = build_eval_fixture(None);

    let weak = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_WEAK,
            "intent": "explain",
            "scope": "code",
            "limit": 5,
            "graph_reasoning": { "paths": false, "communities": false, "risk": false }
        }),
    )
    .await;
    assert_eq!(weak["confidence"], "low");
    assert_eq!(weak["answerable"], true);

    let medium = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_MEDIUM,
            "intent": "explain",
            "scope": "code",
            "limit": 5,
            "graph_reasoning": { "paths": false, "communities": false, "risk": false }
        }),
    )
    .await;
    assert_eq!(medium["confidence"], "medium");

    let strong = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_STRONG,
            "intent": "review",
            "scope": "all",
            "limit": 5,
            "graph_reasoning": { "paths": false, "communities": false, "risk": false }
        }),
    )
    .await;
    assert_eq!(strong["confidence"], "high");
}

#[tokio::test]
#[ignore = "TODO(phase4): port main kcp2 graph-path refinements into spur-analyst handler"]
async fn disjoint_singletons_emit_single_no_path_caveat() {
    let fixture = build_eval_fixture(None);

    let pack = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_DISJOINT,
            "intent": "review",
            "scope": "code",
            "limit": 5,
            "graph_reasoning": {
                "paths": true,
                "communities": false,
                "risk": false,
                "max_path_hops": 2,
                "max_paths": 3
            }
        }),
    )
    .await;

    assert_eq!(pack["graph_paths"][0]["status"], "no_path");
    assert_eq!(pack["graph_paths"][0]["traversal"], "undirected");
    assert_eq!(
        pack["graph_paths"][0]["rows"]
            .as_array()
            .expect("rows")
            .len(),
        0
    );
    let graph_path_caveats = caveats_with_code(&pack, "graph_path_unavailable");
    assert_eq!(
        graph_path_caveats.len(),
        1,
        "caveat should dedupe per source"
    );
    assert_eq!(
        graph_path_caveats[0]["stable_symbol_id"],
        id(&fixture.ids, "isolated_one")
    );
}

#[tokio::test]
async fn scope_and_intent_variations_drive_defaults() {
    let fixture = build_eval_fixture(None);

    let all = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_SCOPE,
            "intent": "plan",
            "scope": "all",
            "limit": 5,
            "graph_reasoning": { "paths": false, "communities": false, "risk": false }
        }),
    )
    .await;
    assert_eq!(
        all["primary_evidence"].as_array().expect("primary").len(),
        1
    );
    assert_eq!(all["supporting_docs"].as_array().expect("docs").len(), 1);

    let docs = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_SCOPE,
            "intent": "review",
            "scope": "docs",
            "limit": 5
        }),
    )
    .await;
    assert!(docs["primary_evidence"]
        .as_array()
        .expect("primary")
        .is_empty());
    assert_eq!(docs["supporting_docs"].as_array().expect("docs").len(), 1);
    assert!(docs["graph_paths"].as_array().expect("paths").is_empty());
    assert!(docs["risk_scorecard"].as_array().expect("risk").is_empty());

    let code = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_SCOPE,
            "intent": "explain",
            "scope": "code",
            "limit": 5,
            "graph_reasoning": { "paths": false, "communities": false, "risk": false }
        }),
    )
    .await;
    assert_eq!(
        code["primary_evidence"].as_array().expect("primary").len(),
        1
    );
    assert!(code["supporting_docs"].as_array().expect("docs").is_empty());

    let graph = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_SCOPE,
            "intent": "explain",
            "scope": "graph",
            "limit": 5,
            "graph_reasoning": { "paths": false, "communities": false, "risk": false }
        }),
    )
    .await;
    assert_eq!(
        graph["primary_evidence"].as_array().expect("primary").len(),
        1
    );
    assert!(graph["supporting_docs"]
        .as_array()
        .expect("docs")
        .is_empty());

    let review_defaults = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_CONNECTED,
            "intent": "review",
            "scope": "code",
            "limit": 5
        }),
    )
    .await;
    assert_eq!(review_defaults["graph_paths"][0]["status"], "path_found");
    assert!(
        !review_defaults["risk_scorecard"]
            .as_array()
            .expect("risk")
            .is_empty(),
        "review/code defaults should include risk"
    );

    let explain_defaults = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_CONNECTED,
            "intent": "explain",
            "scope": "code",
            "limit": 5
        }),
    )
    .await;
    assert!(
        explain_defaults["graph_paths"]
            .as_array()
            .expect("paths")
            .is_empty(),
        "explain defaults should not request paths"
    );
}

#[tokio::test]
async fn stale_analyst_hash_suppresses_graph_reasoning_sections() {
    let fixture = build_eval_fixture(Some("stale-analyst-hash"));

    let pack = call_pack(
        &fixture.root,
        json!({
            "query": QUERY_CONNECTED,
            "intent": "review",
            "scope": "code",
            "limit": 5,
            "graph_reasoning": {
                "paths": true,
                "communities": true,
                "risk": true,
                "max_path_hops": 3,
                "max_paths": 4
            }
        }),
    )
    .await;

    assert_eq!(pack["staleness"]["exact_graph_verified"], true);
    assert_eq!(
        pack["staleness"]["analyst_graph_content_hash"],
        "stale-analyst-hash"
    );
    assert_eq!(pack["staleness"]["exact_graph_hash"], fixture.graph_hash);
    assert_eq!(pack["staleness"]["analyst_matches_exact_graph"], false);
    assert!(pack["graph_paths"].as_array().expect("paths").is_empty());
    assert!(pack["risk_scorecard"].as_array().expect("risk").is_empty());
    assert!(pack["community_context"]
        .as_array()
        .expect("communities")
        .is_empty());
    let stale = caveats_with_code(&pack, "analyst_graph_stale");
    assert_eq!(stale.len(), 1);
    assert!(
        stale[0]["message"]
            .as_str()
            .expect("stale message")
            .contains("graph reasoning skipped"),
        "stale caveat should explain suppression: {stale:#?}"
    );
}
