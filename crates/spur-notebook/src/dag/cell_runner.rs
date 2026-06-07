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
    let kernelspec = metadata
        .get("kernelspec")
        .and_then(kernelspec_name)
        .or_else(|| spur.get("kernelspec").and_then(kernelspec_name))
        .or_else(|| {
            spur.get("code_type")
                .and_then(Value::as_str)
                .and_then(code_type_kernelspec)
        })
        .or(root_kernelspec)
        .unwrap_or_else(|| "python3".to_owned());
    let consumed = dag
        .get("consumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let produced = dag
        .get("produces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(produced_port_name);

    Ok(CellRouting {
        kernelspec,
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
        "spur" => Some("spur".to_owned()),
        _ => None,
    }
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
                        })
                        .collect(),
                    consumes: consumes.into_iter().map(str::to_owned).collect(),
                    source: None,
                }),
                code_type: None,
                frontend: None,
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
