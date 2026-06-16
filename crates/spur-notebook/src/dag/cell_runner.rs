use std::{collections::HashMap, future::Future, path::Path, pin::Pin, sync::Arc};

use arrow_array::{RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use jute::backend::notebook::{kernelspec_for as jute_kernelspec_for, CodeType};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::dag::ai::{render_port_context, AiError, AiNodeBackend, AiRunRequest};
use crate::dag::engine::{
    CellRunOutcome, CellRunRequest, CellRunStatus, CellRunner, EngineError, KernelEnsureRequest,
    RunCellCommandRunner,
};
use crate::dag::{notebook_port_root, PortStore};

#[derive(Clone)]
pub struct NotebookCellRunner<R = RunCellCommandRunner> {
    inner: R,
    ai: Arc<dyn AiNodeBackend>,
    cache: Arc<Mutex<HashMap<String, String>>>,
    backend_id: String,
}

impl<R> NotebookCellRunner<R>
where
    R: CellRunner,
{
    pub fn new_with_inner(inner: R, ai: Arc<dyn AiNodeBackend>) -> Self {
        let backend_id = format!("{:p}", Arc::as_ptr(&ai));
        Self {
            inner,
            ai,
            cache: Arc::new(Mutex::new(HashMap::new())),
            backend_id,
        }
    }
}

impl NotebookCellRunner<RunCellCommandRunner> {
    pub fn new(inner: RunCellCommandRunner, ai: Arc<dyn AiNodeBackend>) -> Self {
        Self::new_with_inner(inner, ai)
    }
}

impl<R> CellRunner for NotebookCellRunner<R>
where
    R: CellRunner,
{
    fn run_cell<'a>(
        &'a self,
        request: CellRunRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>> {
        Box::pin(async move {
            let routing = resolve_cell_routing(&request)?;
            if routing.kernelspec != "spur" {
                let request = prepare_cell_source(request, &routing);
                return self.inner.run_cell(request).await;
            }

            let produced = routing.produced.ok_or(AiError::NoOutputPort)?;
            let mut store = PortStore::open_at(port_root(&request))?;
            let mut context = Vec::with_capacity(routing.consumed.len());
            let mut port_versions = Vec::with_capacity(routing.consumed.len());

            for port in &routing.consumed {
                let read = store.get(port)?;
                port_versions.push((port.clone(), read.version()));
                context.push(render_port_context(port, &read));
            }

            let cache_key = input_hash(&request.code, &port_versions, &self.backend_id);
            if let Some(cached) = self.cache.lock().await.get(&cache_key).cloned() {
                write_text_port(&mut store, &produced, &cached)?;
                return Ok(CellRunOutcome {
                    status: CellRunStatus::Succeeded,
                });
            }

            let output = self
                .ai
                .run(AiRunRequest {
                    cell_id: request.cell_id.clone(),
                    prompt: request.code,
                    context,
                    cancel: CancellationToken::new(),
                })
                .await?;
            write_text_port(&mut store, &produced, &output.text)?;
            self.cache.lock().await.insert(cache_key, output.text);

            Ok(CellRunOutcome {
                status: CellRunStatus::Succeeded,
            })
        })
    }

    fn ensure_kernel<'a>(
        &'a self,
        request: KernelEnsureRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), EngineError>> + Send + 'a>> {
        Box::pin(async move {
            if request.spec_name == "spur" {
                Ok(())
            } else {
                self.inner.ensure_kernel(request).await
            }
        })
    }
}

impl From<AiError> for EngineError {
    fn from(error: AiError) -> Self {
        Self::RunCell(error.to_string())
    }
}

struct CellRouting {
    kernelspec: String,
    code_type: Option<String>,
    consumed: Vec<String>,
    produced: Option<String>,
}

fn resolve_cell_routing(request: &CellRunRequest) -> Result<CellRouting, EngineError> {
    let bytes = std::fs::read(&request.notebook_path)
        .map_err(|error| EngineError::RunCell(format!("read notebook metadata: {error}")))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|error| EngineError::RunCell(format!("parse notebook metadata: {error}")))?;
    let root_kernelspec = root
        .get("metadata")
        .and_then(|metadata| metadata.get("kernelspec"))
        .and_then(kernelspec_name);
    let cell = root
        .get("cells")
        .and_then(Value::as_array)
        .and_then(|cells| {
            cells.iter().find(|cell| {
                cell.get("id").and_then(Value::as_str) == Some(request.cell_id.as_str())
            })
        })
        .ok_or_else(|| EngineError::CellNotFound {
            cell_id: request.cell_id.clone(),
        })?;

    let metadata = cell.get("metadata").unwrap_or(&Value::Null);
    let spur = metadata.get("spur").unwrap_or(&Value::Null);
    let dag = spur.get("dag").unwrap_or(&Value::Null);
    let code_type = spur
        .get("code_type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let kernelspec = metadata
        .get("kernelspec")
        .and_then(kernelspec_name)
        .or_else(|| spur.get("kernelspec").and_then(kernelspec_name))
        .or_else(|| code_type.as_deref().and_then(code_type_kernelspec))
        .or(root_kernelspec)
        .unwrap_or_else(|| "python3".to_owned());
    let mut consumed: Vec<String> = dag
        .get("consumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    // SQL cells declare their dependencies implicitly via FROM/JOIN. Derive
    // those relations from the cell source and union them into `consumed` so the
    // DAG tracks lineage and cascade ordering without manual `dag.consumes`.
    if code_type.as_deref() == Some("sql") {
        for relation in sql_referenced_relations(&request.code) {
            if !consumed.contains(&relation) {
                consumed.push(relation);
            }
        }
    }
    let produced = dag
        .get("produces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(produced_port_name);

    Ok(CellRouting {
        kernelspec,
        code_type,
        consumed,
        produced,
    })
}

fn kernelspec_name(value: &Value) -> Option<String> {
    match value {
        Value::String(name) => Some(name.clone()),
        Value::Object(object) => object
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn code_type_kernelspec(code_type: &str) -> Option<String> {
    match code_type {
        "python" => Some(jute_kernelspec_for(CodeType::Python).to_owned()),
        "javascript" => Some(jute_kernelspec_for(CodeType::Javascript).to_owned()),
        "rust" => Some(jute_kernelspec_for(CodeType::Rust).to_owned()),
        "go" => Some(jute_kernelspec_for(CodeType::Go).to_owned()),
        "sql" => Some(jute_kernelspec_for(CodeType::Sql).to_owned()),
        "spur" => Some("spur".to_owned()),
        _ => None,
    }
}

fn prepare_cell_source(mut request: CellRunRequest, routing: &CellRouting) -> CellRunRequest {
    if routing.code_type.as_deref() == Some("sql") {
        request.code = transpile_sql_cell(&request.code, routing.produced.as_deref());
    }
    request
}

/// Wrap a DuckDB SQL cell into Python executed on the shared default connection.
/// When `produced` is set, the Arrow result binds to that kernel global so the
/// existing produced-port capture publishes it. Otherwise the query runs for its
/// side effects / preview only.
fn transpile_sql_cell(sql: &str, produced: Option<&str>) -> String {
    let literal = sql.replace("\"\"\"", "\\\"\\\"\\\"");
    let bootstrap = sql_cell_duckdb_bootstrap();
    match produced {
        Some(name) => {
            format!("{bootstrap}{name} = duckdb.sql(r\"\"\"{literal}\"\"\").arrow()\n{name}")
        }
        None => format!("{bootstrap}duckdb.sql(r\"\"\"{literal}\"\"\")"),
    }
}

fn sql_cell_duckdb_bootstrap() -> String {
    let extension_path = crate::extension_install::extension_install_dir()
        .join(crate::extension_install::loaded_extension_filename())
        .display()
        .to_string();
    let extension_path = serde_json::to_string(&extension_path)
        .unwrap_or_else(|_| "\"~/.spur/extensions/spur_rest.duckdb_extension\"".to_owned());

    format!(
        r#"import duckdb
_SPUR_DUCKDB_EXTENSION_PATH = {extension_path}
_SPUR_DUCKDB_EXTENSION_SQL = _SPUR_DUCKDB_EXTENSION_PATH.replace("'", "''")

if "_SPUR_DUCKDB_CONNECTION" not in globals():
    _SPUR_DUCKDB_CONNECTION = duckdb.connect(
        database=":memory:",
        config={{"allow_unsigned_extensions": "true"}},
    )

duckdb.set_default_connection(_SPUR_DUCKDB_CONNECTION)
if not globals().get("_SPUR_REST_EXTENSION_LOADED", False):
    duckdb.sql(f"LOAD '{{_SPUR_DUCKDB_EXTENSION_SQL}}'")
    _SPUR_REST_EXTENSION_LOADED = True

"#
    )
}

/// Conservative lineage helper: the identifier token immediately after a `FROM`
/// or `JOIN` keyword (case-insensitive). Dotted names (`ds.events`) are kept
/// whole and duplicates are removed. This is not a full SQL parser; it is good
/// enough to wire DAG dependency edges for a SQL cell without pulling in a SQL
/// parser dependency.
fn sql_referenced_relations(sql: &str) -> Vec<String> {
    let mut relations: Vec<String> = Vec::new();
    let mut expect_relation = false;
    for token in sql.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')') {
        let word = token.trim();
        if word.is_empty() {
            continue;
        }
        if expect_relation {
            let name = word.trim_end_matches(';');
            let is_ident = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
            if is_ident && !relations.iter().any(|existing| existing == name) {
                relations.push(name.to_owned());
            }
            expect_relation = false;
            continue;
        }
        let upper = word.to_ascii_uppercase();
        if upper == "FROM" || upper == "JOIN" {
            expect_relation = true;
        }
    }
    relations
}

fn produced_port_name(value: &Value) -> Option<String> {
    match value {
        Value::String(port) => Some(port.clone()),
        Value::Object(object) => object
            .get("port")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn port_root(request: &CellRunRequest) -> impl AsRef<Path> {
    notebook_port_root(&request.notebook_path)
}

fn write_text_port(store: &mut PortStore, port: &str, text: &str) -> Result<(), EngineError> {
    let schema = Arc::new(Schema::new(vec![Field::new("text", DataType::Utf8, false)]));
    let batch = RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec![text]))])
        .map_err(|error| EngineError::RunCell(format!("build ai output port: {error}")))?;
    store.put(port, &batch)?;
    Ok(())
}

fn input_hash(code: &str, port_versions: &[(String, u64)], backend_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(backend_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(code.as_bytes());
    for (port, version) in port_versions {
        hasher.update(b"\0");
        hasher.update(port.as_bytes());
        hasher.update(b"=");
        hasher.update(version.to_string().as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        future::Future,
        path::Path,
        pin::Pin,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use arrow_array::{RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use jute::backend::notebook::{
        Cell, CellDagMetadata, CellMetadata, CodeCell, MultilineString, NotebookMetadata,
        NotebookRoot, PortSpec, SpurCellMetadata,
    };
    use serde_json::json;
    use tempfile::TempDir;

    use crate::dag::ai::{AiError, AiNodeBackend, AiRunOutput, AiRunRequest};
    use crate::dag::engine::{
        CellRunOutcome, CellRunRequest, CellRunStatus, CellRunner, EngineError, KernelEnsureRequest,
    };
    use crate::dag::{notebook_port_root, PortStore};

    #[derive(Default)]
    struct FakeAiBackend {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AiNodeBackend for FakeAiBackend {
        async fn run(&self, req: AiRunRequest) -> Result<AiRunOutput, AiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req.cell_id, "ai");
            assert_eq!(req.prompt, "summarize sales");
            assert_eq!(req.context.len(), 1);
            Ok(AiRunOutput {
                text: "ANSWER".to_owned(),
                usage: None,
            })
        }
    }

    #[derive(Clone, Default)]
    struct FakeInnerRunner {
        requests: Arc<Mutex<Vec<CellRunRequest>>>,
    }

    impl CellRunner for FakeInnerRunner {
        fn run_cell<'a>(
            &'a self,
            request: CellRunRequest,
        ) -> Pin<Box<dyn Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.requests.lock().expect("requests").push(request);
                Ok(CellRunOutcome {
                    status: CellRunStatus::Succeeded,
                })
            })
        }

        fn ensure_kernel<'a>(
            &'a self,
            _request: KernelEnsureRequest,
        ) -> Pin<Box<dyn Future<Output = Result<(), EngineError>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn spur_cell_calls_backend_and_writes_output_then_caches() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ai.ipynb");
        write_notebook(
            &notebook_path,
            vec![cell(
                "ai",
                "spur",
                "summarize sales",
                vec!["answer"],
                vec!["sales"],
            )],
        );
        put_text_port(&notebook_path, "sales", "region,total\nwest,42");

        let backend = Arc::new(FakeAiBackend::default());
        let runner =
            NotebookCellRunner::new_with_inner(FakeInnerRunner::default(), backend.clone());
        let request = request(&notebook_path, "ai", "summarize sales");

        let first = runner.run_cell(request.clone()).await.expect("first run");
        assert_eq!(first.status, CellRunStatus::Succeeded);
        assert_output_port(&notebook_path, "answer", "ANSWER", 1);

        let second = runner.run_cell(request).await.expect("second run");
        assert_eq!(second.status, CellRunStatus::Succeeded);
        assert_output_port(&notebook_path, "answer", "ANSWER", 2);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn non_spur_cell_delegates_to_inner_runner() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("python.ipynb");
        write_notebook(
            &notebook_path,
            vec![cell("py", "python", "print('ok')", vec!["out"], Vec::new())],
        );

        let backend = Arc::new(FakeAiBackend::default());
        let inner = FakeInnerRunner::default();
        let requests = inner.requests.clone();
        let runner = NotebookCellRunner::new_with_inner(inner, backend.clone());

        let outcome = runner
            .run_cell(request(&notebook_path, "py", "print('ok')"))
            .await
            .expect("delegated run");

        assert_eq!(outcome.status, CellRunStatus::Succeeded);
        assert_eq!(requests.lock().expect("requests").len(), 1);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn code_type_kernelspec_maps_sql_to_python3() {
        assert_eq!(code_type_kernelspec("sql").as_deref(), Some("python3"));
    }

    #[test]
    fn transpile_sql_binds_produced_relation_as_arrow() {
        let py = transpile_sql_cell("SELECT 1 AS x", Some("answer"));
        assert!(py.contains("import duckdb"));
        assert!(py.contains("answer ="));
        assert!(py.contains(".arrow()"));
        assert!(py.contains("SELECT 1 AS x"));
    }

    #[test]
    fn transpile_sql_runs_anonymously_without_produced() {
        let py = transpile_sql_cell("SELECT 1", None);
        assert!(py.contains("duckdb.sql("));
        assert!(!py.contains(" = duckdb.sql"));
    }

    #[test]
    fn transpile_sql_bootstraps_spur_rest_extension_before_query() {
        let py = transpile_sql_cell("SELECT * FROM polymarket_markets() LIMIT 100", None);

        let bootstrap = py
            .find("allow_unsigned_extensions")
            .expect("bootstrap enables unsigned DuckDB extensions");
        let load = py
            .find("spur_rest.duckdb_extension")
            .expect("bootstrap references bundled REST extension");
        let query = py
            .find("SELECT * FROM polymarket_markets() LIMIT 100")
            .expect("query is present");

        assert!(bootstrap < query);
        assert!(load < query);
        assert!(py.contains("duckdb.set_default_connection"));
    }

    #[test]
    fn sql_referenced_relations_extracts_from_and_join() {
        let rels = sql_referenced_relations(
            "SELECT * FROM matches m JOIN ds.events e USING (id) WHERE e.type = 'Goal'",
        );
        assert!(rels.iter().any(|r| r == "matches"));
        assert!(rels.iter().any(|r| r == "ds.events"));
        // de-duplicated and no stray keywords captured.
        assert_eq!(rels.iter().filter(|r| *r == "matches").count(), 1);
        assert!(!rels.iter().any(|r| r.eq_ignore_ascii_case("join")));
    }

    #[test]
    fn sql_referenced_relations_handles_lowercase_and_no_tables() {
        assert!(sql_referenced_relations("SELECT 1 AS x").is_empty());
        let rels = sql_referenced_relations("select * from top_scorers where goals >= 5");
        assert_eq!(rels, vec!["top_scorers".to_owned()]);
    }

    #[tokio::test]
    async fn sql_cell_delegates_transpiled_source_to_inner_runner() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("sql.ipynb");
        let mut sql = cell(
            "sql",
            "python3",
            "SELECT 1 AS x",
            vec!["answer"],
            Vec::new(),
        );
        let Cell::Code(code) = &mut sql else {
            panic!("expected code cell");
        };
        code.metadata
            .spur
            .as_mut()
            .expect("spur metadata")
            .code_type = Some(CodeType::Sql);
        write_notebook(&notebook_path, vec![sql]);

        let backend = Arc::new(FakeAiBackend::default());
        let inner = FakeInnerRunner::default();
        let requests = inner.requests.clone();
        let runner = NotebookCellRunner::new_with_inner(inner, backend.clone());

        let outcome = runner
            .run_cell(request(&notebook_path, "sql", "SELECT 1 AS x"))
            .await
            .expect("delegated run");

        assert_eq!(outcome.status, CellRunStatus::Succeeded);
        let requests = requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].code.contains("answer ="));
        assert!(requests[0].code.contains("duckdb.sql("));
        assert!(requests[0].code.contains(".arrow()"));
        assert!(requests[0].code.contains("SELECT 1 AS x"));
        assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    }

    fn request(notebook_path: &Path, cell_id: &str, code: &str) -> CellRunRequest {
        CellRunRequest {
            cell_id: cell_id.to_owned(),
            notebook_path: notebook_path.to_string_lossy().into_owned(),
            kernel_id: Some("kernel".to_owned()),
            code: code.to_owned(),
            expected_version: 1,
        }
    }

    fn write_notebook(path: &Path, cells: Vec<Cell>) {
        let root = NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells,
        };
        std::fs::write(path, serde_json::to_vec(&root).expect("notebook json"))
            .expect("write notebook");
    }

    fn cell(
        id: &str,
        kernelspec: &str,
        source: &str,
        produces: Vec<&str>,
        consumes: Vec<&str>,
    ) -> Cell {
        let mut metadata = CellMetadata {
            spur: Some(SpurCellMetadata {
                version: 1,
                last_edited_by: None,
                datasource_setup: None,
                dag: Some(CellDagMetadata {
                    produces: produces
                        .into_iter()
                        .map(|port| PortSpec {
                            port: port.to_owned(),
                            repr: "arrow".to_owned(),
                            display: None,
                            class: None,
                            schema: None,
                        })
                        .collect(),
                    consumes: consumes.into_iter().map(str::to_owned).collect(),
                    source: None,
                }),
                code_type: None,
                frontend: None,
                cron: None,
            }),
            jute_deck: None,
            other: Default::default(),
        };
        metadata
            .other
            .insert("kernelspec".to_owned(), json!({ "name": kernelspec }));

        Cell::Code(CodeCell {
            id: Some(id.to_owned()),
            metadata,
            source: MultilineString::Single(source.to_owned()),
            execution_count: None,
            outputs: Vec::new(),
        })
    }

    fn put_text_port(notebook_path: &Path, port: &str, text: &str) {
        let mut store = PortStore::open_at(notebook_port_root(notebook_path)).expect("port store");
        let batch = text_batch("value", text);
        store.put(port, &batch).expect("put port");
    }

    fn assert_output_port(notebook_path: &Path, port: &str, expected: &str, version: u64) {
        let store =
            PortStore::open_read_only_at(notebook_port_root(notebook_path)).expect("port store");
        let read = store.get(port).expect("read output");
        let crate::dag::PortRead::Arrow {
            version: actual_version,
            batches,
            ..
        } = read
        else {
            panic!("expected Arrow output port");
        };
        assert_eq!(actual_version, version);
        let column = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("utf8 column");
        assert_eq!(column.value(0), expected);
    }

    fn text_batch(column: &str, value: &str) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new(column, DataType::Utf8, false)]));
        RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(vec![value]))])
            .expect("record batch")
    }
}
