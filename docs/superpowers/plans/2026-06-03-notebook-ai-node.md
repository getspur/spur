# Notebook AI Node (Tier 1) Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-notebook-ai-node-design.ipynb`
**Design epic:** _(brainstorming was lightweight; no separate design epic issue)_

**Goal:** Add a Tier-1 "AI node" to the jute-notebook reactive DAG engine — a cell with kernelspec `spur` whose body executes against Spur (via a thin `AgentConnection`) instead of a kernel, reading upstream Arrow ports as context and writing its text answer to an output port.

**Architecture:** A new `AiNodeBackend` trait is the sole AI seam the engine knows about. The Tier-1 impl `AcpAgentBackend` wraps an injected `dyn AgentConnection` (spur-acp) and drains a prompt turn into text. A composite `NotebookCellRunner` routes cells by kernelspec — non-AI cells take the existing kernel path untouched; `spur` cells take the AI path (read consumed ports → render context → backend → write output port), with an always-on input-hash cache. The engine gains a `Stale` status and a manual/live cascade rule. All backend/runner logic is testable with fakes; only the final task touches live connection construction.

**Tech Stack:** Rust, `spur-notebook` crate, `spur-acp` (`AgentConnection`), Arrow IPC (`PortStore`), tokio, async-trait.

---

## File Structure Mapping

- **Create** `crates/spur-notebook/src/dag/ai/mod.rs` — `AiNodeBackend` trait + `AiRunRequest`, `AiRunOutput`, `AiUsage`, `PortContext`, `AiError` (Task 1).
- **Create** `crates/spur-notebook/src/dag/ai/context.rs` — Arrow `PortRead` → `PortContext` rendering (Task 3).
- **Create** `crates/spur-notebook/src/dag/ai/acp_backend.rs` — `AcpAgentBackend` (Task 2).
- **Create** `crates/spur-notebook/src/dag/cell_runner.rs` — composite `NotebookCellRunner` + input-hash cache (Task 4).
- **Modify** `crates/spur-notebook/src/dag/mod.rs` — `pub mod ai; pub mod cell_runner;` + re-exports (Tasks 1, 3, 4).
- **Modify** `crates/spur-notebook/src/dag/engine.rs` — add `CellRunStatus::Stale` + manual/live cascade rule (Task 5).
- **Modify** `crates/spur-notebook/src/dag/run_context.rs` — construct a live `AgentConnection` + `AcpAgentBackend` and inject into `NotebookCellRunner` (Task 6).

## Dependency DAG

```
T1 ──┬─> T2 ──────────────┐
     ├─> T3 ──> T4 ──> T5  │
     │         └────────────> T6  (T6 depends on T2 + T4)
```

- **T1** root (types). **T2** and **T3** parallel after T1. **T4** after T1+T3. **T5** after T4 (same engine cascade area). **T6** after T2+T4.

---

### Task 1: AI node backend trait + value types

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-notebook/src/dag/ai/mod.rs`
- Modify: `crates/spur-notebook/src/dag/mod.rs` (add `pub mod ai;` and re-exports)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `cargo build -p spur-notebook` compiles.
- [ ] `AiNodeBackend`, `AiRunRequest`, `AiRunOutput`, `AiUsage`, `PortContext`, `AiError` are public from `crate::dag::ai`.
- [ ] `cargo test -p spur-notebook ai::mod` passes (the trait-object smoke test below).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the new `ai/mod.rs` file and the two-line `mod.rs` wiring.
- OUT of scope: `engine.rs`, `acp_backend.rs`, any connection logic.
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** (append to `ai/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct EchoBackend;

    #[async_trait::async_trait]
    impl AiNodeBackend for EchoBackend {
        async fn run(&self, req: AiRunRequest) -> Result<AiRunOutput, AiError> {
            Ok(AiRunOutput { text: req.prompt, usage: None })
        }
    }

    #[tokio::test]
    async fn backend_trait_object_runs() {
        let backend: std::sync::Arc<dyn AiNodeBackend> = std::sync::Arc::new(EchoBackend);
        let out = backend
            .run(AiRunRequest {
                cell_id: "c1".into(),
                prompt: "hello".into(),
                context: vec![PortContext { port: "df".into(), rendered: "a,b".into() }],
                cancel: CancellationToken::new(),
            })
            .await
            .unwrap();
        assert_eq!(out.text, "hello");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-notebook backend_trait_object_runs -- --nocapture`
Expected: FAIL — `AiNodeBackend` / types not defined.

- [ ] **Step 3: Write minimal implementation** (top of `ai/mod.rs`)

```rust
//! AI-node backend seam for the reactive DAG engine (Tier 1).
use tokio_util::sync::CancellationToken;

/// One consumed upstream port, already rendered to text for prompt context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortContext {
    pub port: String,
    pub rendered: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug)]
pub struct AiRunRequest {
    pub cell_id: String,
    /// The cell body (the prompt).
    pub prompt: String,
    /// Rendered consumed ports, injected as context.
    pub context: Vec<PortContext>,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiRunOutput {
    pub text: String,
    pub usage: Option<AiUsage>,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("agent connection init failed: {0}")]
    Init(String),
    #[error("prompt turn failed: {0}")]
    Prompt(String),
    #[error("ai node run timed out")]
    Timeout,
    #[error("ai node run cancelled")]
    Cancelled,
    #[error("ai node has no produced text port declared")]
    NoOutputPort,
}

/// The only AI abstraction the engine knows about. Tier-2 (session/Orchestrator)
/// becomes a second impl with no engine change.
#[async_trait::async_trait]
pub trait AiNodeBackend: Send + Sync {
    async fn run(&self, req: AiRunRequest) -> Result<AiRunOutput, AiError>;
}
```

Add to `crates/spur-notebook/src/dag/mod.rs`:

```rust
pub mod ai;
pub use ai::{AiError, AiNodeBackend, AiRunOutput, AiRunRequest, AiUsage, PortContext};
```

Ensure `async-trait`, `thiserror`, and `tokio-util` are in `spur-notebook/Cargo.toml` (they are workspace deps already used elsewhere in the crate; add the entry if missing — that is the only Cargo.toml change permitted in this task).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-notebook backend_trait_object_runs -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/ai/mod.rs crates/spur-notebook/src/dag/mod.rs crates/spur-notebook/Cargo.toml
git commit -m "feat(notebook): AiNodeBackend trait and value types for AI node"
```

---

### Task 2: `AcpAgentBackend` — drain a prompt turn into text

**Task ID:** `task-2`

**Files:**
- Create: `crates/spur-notebook/src/dag/ai/acp_backend.rs`
- Modify: `crates/spur-notebook/src/dag/ai/mod.rs` (add `pub mod acp_backend;` + re-export)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] `AcpAgentBackend` implements `AiNodeBackend`.
- [ ] It lazily `initialize()` + `new_session()` once, then `prompt()`, draining `SessionUpdate::AgentMessageChunk` text into `AiRunOutput.text`.
- [ ] `cargo test -p spur-notebook acp_backend` passes (fake-connection test below).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `ai/acp_backend.rs` + the module wiring line in `ai/mod.rs`.
- OUT of scope: `engine.rs`, `cell_runner.rs`, `run_context.rs`, real process spawning. The backend takes an **already-constructed** `Arc<Mutex<dyn AgentConnection>>` — it must not build a connection itself.
- If you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Reference (read before implementing):** `crates/spur-acp/src/connection/mod.rs` (`AgentConnection` trait, `prompt` returns `Stream<SessionNotification>`); text shape confirmed in `crates/spur-acp/src/connection/cli_wrap_adapter.rs:223` — `SessionUpdate::AgentMessageChunk(ContentChunk(ContentBlock::Text(TextContent{ text })))`.

**Implementation:**

- [ ] **Step 1: Write the failing test** (in `acp_backend.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::ai::{AiNodeBackend, AiRunRequest};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    // Minimal in-test AgentConnection that yields two AgentMessageChunk lines.
    // (Construct SessionNotification exactly as cli_wrap_adapter.rs does.)
    // ... build `FakeConn` implementing spur_acp::connection::AgentConnection,
    //     returning a stream of ["Hello, ", "world"] text chunks from prompt().

    #[tokio::test]
    async fn drains_prompt_stream_to_text() {
        let conn: Arc<Mutex<dyn spur_acp::connection::AgentConnection>> =
            Arc::new(Mutex::new(FakeConn::with_lines(["Hello, ", "world"])));
        let backend = AcpAgentBackend::new(conn, std::path::PathBuf::from("/tmp/nb"));
        let out = backend
            .run(AiRunRequest {
                cell_id: "c1".into(),
                prompt: "say hi".into(),
                context: vec![],
                cancel: CancellationToken::new(),
            })
            .await
            .unwrap();
        assert_eq!(out.text, "Hello, world");
    }
}
```

Implement `FakeConn` in the test module against the real `AgentConnection` trait (most methods can be trivial; `prompt` returns a `futures::stream::iter` of two `SessionNotification`s built like `cli_wrap_adapter.rs:223-227`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-notebook drains_prompt_stream_to_text -- --nocapture`
Expected: FAIL — `AcpAgentBackend` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::Mutex;

use agent_client_protocol::schema::{ContentBlock, SessionUpdate};
use spur_acp::connection::AgentConnection;
use spur_acp::types::{InitializeRequest, PromptRequest};

use crate::dag::ai::{AiError, AiNodeBackend, AiRunOutput, AiRunRequest};

/// Tier-1 AI backend: one ACP session per notebook, one prompt turn per run.
pub struct AcpAgentBackend {
    conn: Arc<Mutex<dyn AgentConnection>>,
    cwd: PathBuf,
    session_id: Mutex<Option<String>>,
}

impl AcpAgentBackend {
    pub fn new(conn: Arc<Mutex<dyn AgentConnection>>, cwd: PathBuf) -> Self {
        Self { conn, cwd, session_id: Mutex::new(None) }
    }

    async fn ensure_session(&self) -> Result<String, AiError> {
        let mut sid = self.session_id.lock().await;
        if let Some(existing) = sid.as_ref() {
            return Ok(existing.clone());
        }
        let mut conn = self.conn.lock().await;
        conn.initialize(InitializeRequest::default())
            .await
            .map_err(|e| AiError::Init(e.to_string()))?;
        let resp = conn
            .new_session(self.cwd.clone(), Vec::new())
            .await
            .map_err(|e| AiError::Init(e.to_string()))?;
        let id = resp.session_id.0.to_string();
        *sid = Some(id.clone());
        Ok(id)
    }
}

#[async_trait::async_trait]
impl AiNodeBackend for AcpAgentBackend {
    async fn run(&self, req: AiRunRequest) -> Result<AiRunOutput, AiError> {
        let session_id = self.ensure_session().await?;

        // Compose prompt = context blocks + body.
        let mut full = String::new();
        for c in &req.context {
            full.push_str(&format!("## Context: {}\n{}\n\n", c.port, c.rendered));
        }
        full.push_str(&req.prompt);

        let prompt_req = PromptRequest::from_text(&session_id, &full);

        let mut conn = self.conn.lock().await;
        let mut stream = conn
            .prompt(prompt_req)
            .await
            .map_err(|e| AiError::Prompt(e.to_string()))?;

        let mut text = String::new();
        loop {
            tokio::select! {
                _ = req.cancel.cancelled() => {
                    let _ = conn.cancel(&session_id).await;
                    return Err(AiError::Cancelled);
                }
                item = stream.next() => match item {
                    Some(notif) => {
                        if let SessionUpdate::AgentMessageChunk(chunk) = notif.update {
                            if let ContentBlock::Text(t) = chunk.content {
                                text.push_str(&t.text);
                            }
                        }
                    }
                    None => break, // stream closed = turn complete
                }
            }
        }
        Ok(AiRunOutput { text, usage: None })
    }
}
```

> Field/constructor names (`PromptRequest::from_text`, `resp.session_id`, `ContentChunk` access, `InitializeRequest::default`) must be matched to the real spur-acp types as you read them — adjust accessors to compile. If the real API diverges materially (e.g. `prompt` requires a builder you cannot satisfy from text), emit `risk` before improvising.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-notebook drains_prompt_stream_to_text -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/ai/acp_backend.rs crates/spur-notebook/src/dag/ai/mod.rs
git commit -m "feat(notebook): AcpAgentBackend drains a prompt turn into text"
```

**Scope Drift Checkpoint:** if matching the spur-acp `prompt`/`new_session` API forces touching files outside `ai/`, emit `scope_drift`.

---

### Task 3: Render consumed Arrow ports to prompt context

**Task ID:** `task-3`

**Files:**
- Create: `crates/spur-notebook/src/dag/ai/context.rs`
- Modify: `crates/spur-notebook/src/dag/ai/mod.rs` (add `pub mod context;` + re-export `render_port_context`)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] `render_port_context(port: &str, read: &PortRead) -> PortContext` renders an Arrow `PortRead` to a compact CSV-ish text block (header + up to N rows).
- [ ] `cargo test -p spur-notebook ai::context` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `ai/context.rs` + module wiring.
- OUT of scope: `engine.rs`, `cell_runner.rs`, `acp_backend.rs`.

**Reference:** `crates/spur-notebook/src/dag/ports.rs` (`PortRead { schema, batches, .. }`, Arrow `RecordBatch`).

**Implementation:**

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;

    fn sample_read() -> crate::dag::ports::PortRead { /* build a 2-col, 2-row batch */ unimplemented!() }

    #[test]
    fn renders_header_and_rows() {
        let read = sample_read();
        let ctx = render_port_context("df", &read);
        assert_eq!(ctx.port, "df");
        assert!(ctx.rendered.lines().next().unwrap().contains("id"));
        assert!(ctx.rendered.contains("alice"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-notebook renders_header_and_rows -- --nocapture`
Expected: FAIL — `render_port_context` undefined.

- [ ] **Step 3: Write minimal implementation**

```rust
use crate::dag::ai::PortContext;
use crate::dag::ports::PortRead;

const MAX_ROWS: usize = 50;

/// Render an Arrow port to a compact, model-friendly text table:
/// a header line of column names, then up to MAX_ROWS comma-joined rows.
pub fn render_port_context(port: &str, read: &PortRead) -> PortContext {
    let mut out = String::new();
    let cols: Vec<String> = read.schema.fields().iter().map(|f| f.name().clone()).collect();
    out.push_str(&cols.join(","));
    out.push('\n');

    let mut emitted = 0usize;
    'outer: for batch in &read.batches {
        for row in 0..batch.num_rows() {
            if emitted >= MAX_ROWS {
                out.push_str(&format!("... ({} more rows truncated)\n", batch.num_rows() - row));
                break 'outer;
            }
            let cells: Vec<String> = (0..batch.num_columns())
                .map(|c| arrow_cast::display::array_value_to_string(batch.column(c), row).unwrap_or_default())
                .collect();
            out.push_str(&cells.join(","));
            out.push('\n');
            emitted += 1;
        }
    }
    PortContext { port: port.to_string(), rendered: out }
}
```

> Use whatever Arrow value-to-string helper the crate already depends on (`arrow_cast::display` or a manual match). If `arrow-cast` is not a dependency, render with a small per-`DataType` match rather than adding a new crate — do not add new third-party deps; emit `risk` if blocked.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-notebook renders_header_and_rows -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/ai/context.rs crates/spur-notebook/src/dag/ai/mod.rs
git commit -m "feat(notebook): render consumed Arrow ports to AI prompt context"
```

---

### Task 4: `NotebookCellRunner` — kernelspec dispatch + AI path + input-hash cache

**Task ID:** `task-4`

**Files:**
- Create: `crates/spur-notebook/src/dag/cell_runner.rs`
- Modify: `crates/spur-notebook/src/dag/mod.rs` (add `pub mod cell_runner;` + re-export `NotebookCellRunner`)

**Depends on:** task-1, task-3

**Acceptance Criteria:**
- [ ] `NotebookCellRunner` implements `CellRunner` (from `engine.rs`), wrapping `RunCellCommandRunner` + `Arc<dyn AiNodeBackend>`.
- [ ] On a non-`spur` kernelspec it delegates verbatim to the inner runner; on `spur` it reads consumed ports via `PortStore`, renders context (Task 3), calls the backend, writes the produced text port (Arrow utf8, version+1).
- [ ] Identical inputs hit the in-memory cache and skip the backend call (asserted via call count).
- [ ] `cargo test -p spur-notebook cell_runner` passes.

**Suggested Worker:** codex *(higher coordination than T1–T3 — touches the `CellRunner` contract; keep edits inside the new file)*

**Scope Boundary:**
- IN scope: `cell_runner.rs` + the `mod.rs` re-export line.
- OUT of scope: `engine.rs` (do NOT edit it — only import from it), `run_context.rs`, `acp_backend.rs`.
- The kernelspec classifier reads the cell's kernelspec from the notebook store using `cell_id`; if you cannot obtain kernelspec without editing `engine.rs`, emit `scope_drift` rather than widening scope.

**Reference:** `engine.rs` — `CellRunner` (`:108`), `CellRunRequest` (`:62`), `CellRunOutcome`, `CellRunStatus`, `RunCellCommandRunner`; `ports.rs` — `PortStore::read`/write + manifest.

**Implementation:**

- [ ] **Step 1: Write the failing test** (use a `FakeAiBackend` counting calls + a `FakeInnerRunner`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // FakeAiBackend { calls: AtomicUsize } impl AiNodeBackend -> returns "ANSWER", counts calls.
    // Build a NotebookCellRunner over a temp PortStore with one consumed port pre-written.

    #[tokio::test]
    async fn spur_cell_calls_backend_and_writes_output_then_caches() {
        // first run: backend called once, output port written with "ANSWER"
        // second identical run: backend NOT called again (cache hit), output still present
        // assert backend.calls == 1
    }

    #[tokio::test]
    async fn non_spur_cell_delegates_to_inner_runner() {
        // kernelspec "python" -> inner FakeInnerRunner invoked, backend.calls == 0
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-notebook spur_cell_calls_backend -- --nocapture`
Expected: FAIL — `NotebookCellRunner` undefined.

- [ ] **Step 3: Write minimal implementation**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::dag::ai::{render_port_context, AiNodeBackend, AiRunRequest};
use crate::dag::engine::{
    CellRunOutcome, CellRunRequest, CellRunStatus, CellRunner, EngineError, RunCellCommandRunner,
};
use crate::dag::ports::PortStore;

#[derive(Clone)]
pub struct NotebookCellRunner {
    inner: RunCellCommandRunner,
    ai: Arc<dyn AiNodeBackend>,
    cache: Arc<Mutex<HashMap<String, String>>>, // input-hash -> output text
}

impl NotebookCellRunner {
    pub fn new(inner: RunCellCommandRunner, ai: Arc<dyn AiNodeBackend>) -> Self {
        Self { inner, ai, cache: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn is_spur_cell(&self, req: &CellRunRequest) -> bool {
        // Resolve the cell's kernelspec from the notebook store by cell_id;
        // treat kernelspec == "spur" as an AI node. (Read-only lookup.)
        kernelspec_for(req) == Some("spur".to_string())
    }
}

#[async_trait::async_trait]
impl CellRunner for NotebookCellRunner {
    fn run_cell<'a>(
        &'a self,
        request: CellRunRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>>
    {
        Box::pin(async move {
            if !self.is_spur_cell(&request) {
                return self.inner.run_cell(request).await;
            }
            // 1. resolve consumes/produces port names for this cell (notebook meta).
            let (consumed, produced) = resolve_ports(&request)?;
            let produced = produced.ok_or(EngineError::from(crate::dag::ai::AiError::NoOutputPort))?;

            // 2. read consumed ports + render context.
            let store = PortStore::open_at(/* notebook root from request */ port_root(&request))
                .map_err(|e| EngineError::RunCell(e.to_string()))?;
            let mut context = Vec::new();
            let mut key_parts = vec![request.code.clone()];
            for name in &consumed {
                let read = store.read(name).map_err(|e| EngineError::RunCell(e.to_string()))?;
                key_parts.push(format!("{name}:{}", read.version));
                context.push(render_port_context(name, &read));
            }
            let key = blake3_hex(&key_parts.join("|")); // any stable hash; see note

            // 3. cache check.
            if let Some(cached) = self.cache.lock().await.get(&key).cloned() {
                write_text_port(&store, &produced, &cached)?;
                return Ok(CellRunOutcome { status: CellRunStatus::Succeeded });
            }

            // 4. call backend.
            let out = self
                .ai
                .run(AiRunRequest {
                    cell_id: request.cell_id.clone(),
                    prompt: request.code.clone(),
                    context,
                    cancel: tokio_util::sync::CancellationToken::new(),
                })
                .await
                .map_err(EngineError::from)?;

            // 5. write output port + cache.
            write_text_port(&store, &produced, &out.text)?;
            self.cache.lock().await.insert(key, out.text);
            Ok(CellRunOutcome { status: CellRunStatus::Succeeded })
        })
    }

    fn ensure_kernel<'a>(
        &'a self,
        request: crate::dag::engine::KernelEnsureRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), EngineError>> + Send + 'a>>
    {
        // AI nodes need no kernel; delegate to inner for everything else.
        self.inner.ensure_kernel(request)
    }
}
```

Add an `impl From<AiError> for EngineError` (map to `EngineError::RunCell(e.to_string())`) in this file. Implement the small helpers (`kernelspec_for`, `resolve_ports`, `port_root`, `write_text_port`, `blake3_hex`) against the existing notebook store + `PortStore` API; `write_text_port` builds a single-column utf8 `RecordBatch` and calls the `PortStore` write method used by kernels (read `ports.rs` for the exact writer signature). Use an existing hash dep (e.g. `blake3` if present, else `std::collections::hash_map::DefaultHasher`) — do not add a new dep.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-notebook cell_runner -- --nocapture`
Expected: PASS (both tests; backend call count asserted).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/cell_runner.rs crates/spur-notebook/src/dag/mod.rs
git commit -m "feat(notebook): NotebookCellRunner kernelspec dispatch + AI path + cache"
```

**Scope Drift Checkpoint:**
- If kernelspec/port-name resolution requires editing `engine.rs`, emit `scope_drift`.
- If estimated remaining work exceeds the original by >50%, emit `scope_drift`.

---

### Task 5: Engine `Stale` status + manual/live cascade rule

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/src/dag/engine.rs` (add `CellRunStatus::Stale`; in the cascade path, when a dependent is an AI node in manual mode, mark it `Stale` instead of running it)

**Depends on:** task-4

**Acceptance Criteria:**
- [ ] `CellRunStatus` gains a `Stale` variant, surfaced through `run_cell_with_status_events`.
- [ ] When an upstream port changes, an AI node whose cell metadata does NOT set `ai_live: true` is marked `Stale` and its backend is NOT invoked; an `ai_live: true` AI node cascades normally.
- [ ] Existing engine tests still pass; new cascade tests pass.
- [ ] `cargo test -p spur-notebook engine` passes.

**Suggested Worker:** codex *(touches the large `engine.rs`; keep the diff surgical — one enum variant + one cascade branch)*

**Scope Boundary:**
- IN scope: `engine.rs` cascade/status code only.
- OUT of scope: `cell_runner.rs`, `ai/*`, `run_context.rs`. Do not move logic between files.

**Reference:** `engine.rs` — `CellRunStatus`, `run_cell_and_cascade` (`:330`), `run_cell_with_status_events` (`:513`); the existing kernelspec-routing / compile-progress branch shows where cell classification already happens.

**Implementation:**

- [ ] **Step 1: Write the failing test** (in engine.rs `tests`)

```rust
#[tokio::test]
async fn manual_ai_node_marked_stale_not_run_on_upstream_change() {
    // Build engine with a FakeRunner that records run_cell calls.
    // Graph: source cell -> ai cell (kernelspec "spur", no ai_live).
    // Change the source port; run cascade.
    // Assert: ai cell status == Stale AND FakeRunner saw 0 runs for the ai cell.
}

#[tokio::test]
async fn live_ai_node_cascades_on_upstream_change() {
    // Same graph but ai cell metadata ai_live=true.
    // Assert: ai cell IS run (FakeRunner saw 1 run).
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-notebook manual_ai_node_marked_stale -- --nocapture`
Expected: FAIL — `CellRunStatus::Stale` undefined / cascade still runs it.

- [ ] **Step 3: Write minimal implementation**

```rust
// In CellRunStatus:
pub enum CellRunStatus {
    Succeeded,
    Failed,
    Stale, // NEW: AI node in manual mode awaiting explicit run
}

// In the cascade step, before dispatching a dependent cell:
fn cascade_should_mark_stale(meta: &CellMeta) -> bool {
    meta.kernelspec.as_deref() == Some("spur")
        && !meta.bool_flag("ai_live") // default false => manual
}
// if cascade_should_mark_stale(meta) { emit Stale status; skip run_cell; continue; }
```

Wire `Stale` into `run_cell_with_status_events` emission. Use the cell-metadata accessor the engine already uses for kernelspec routing (do not invent a new metadata source).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-notebook engine -- --nocapture`
Expected: PASS (new tests + all pre-existing engine tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/engine.rs
git commit -m "feat(notebook): Stale status + manual/live cascade rule for AI nodes"
```

**Scope Drift Checkpoint:** if making the cascade rule work requires touching `cell_runner.rs` or `ai/*`, emit `scope_drift`.

---

### Task 6: Live wiring — construct `AgentConnection` and inject the backend

**Task ID:** `task-6`

**Files:**
- Modify: `crates/spur-notebook/src/dag/run_context.rs` (build a live `AgentConnection` from an `AgentConfig`, wrap in `AcpAgentBackend`, construct `NotebookCellRunner` instead of the bare `RunCellCommandRunner`)

**Depends on:** task-2, task-4

**Acceptance Criteria:**
- [ ] `notebook_run_context*` constructs a `NotebookCellRunner { inner: RunCellCommandRunner, ai: AcpAgentBackend(connection) }`.
- [ ] The `AgentConfig`/connection is built by mirroring `build_connection_from_transport`; the engine type becomes `ReactiveEngine<NotebookCellRunner>`.
- [ ] An integration test wires a stub `AgentConfig` (or injected fake connection via the `_with_runner` seam) and round-trips one `spur` cell to an output port.
- [ ] `cargo build -p spur-notebook` and `cargo test -p spur-notebook run_context` pass.

**Suggested Worker:** claude-code-acp *(multi-file integration + config plumbing; if routed to codex, watch the scope checkpoints closely)*

**Scope Boundary:**
- IN scope: `run_context.rs` connection/runner construction; the integration test.
- OUT of scope: `engine.rs` internals, `cell_runner.rs` logic, `acp_backend.rs` logic. You only *compose* the pieces built by Tasks 2/4.

**Reference (read before implementing):** `crates/spur-core/src/orchestrator/connection.rs:674` `build_connection_from_transport(config: &spur_acp::config::AgentConfig, spawn_args, permission_tx, repo_root) -> Box<dyn AgentConnection>` — it is `pub(super)`, so replicate its small `match config.transport { Acp => NativeAcpConnection::new_with_kind(...), Stdio => StdioAdapter::new(...), CliWrap => CliWrapAdapter::new(...), StreamJson => StreamJsonAdapter::new(...) }` in the notebook (all four adapter constructors are public in `spur-acp`). Use `config.effective_args()` for `spawn_args`.

**Implementation:**

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn run_context_wires_ai_backend_and_runs_spur_cell() {
    // Use notebook_run_context_with_runner-style injection to supply a
    // NotebookCellRunner built over a fake AgentConnection; create a notebook
    // with one `spur` cell producing port "answer"; run it; assert "answer"
    // port contains the fake agent's text.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-notebook run_context_wires_ai_backend -- --nocapture`
Expected: FAIL — run context still builds a bare `RunCellCommandRunner`.

- [ ] **Step 3: Write minimal implementation**

```rust
fn build_agent_connection(config: &spur_acp::config::AgentConfig, repo_root: &Path)
    -> Box<dyn spur_acp::connection::AgentConnection>
{
    use spur_acp::config::TransportKind;
    use spur_acp::connection::{CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter};
    let args = config.effective_args();
    match config.transport {
        TransportKind::Acp => {
            let mut c = NativeAcpConnection::new_with_kind(
                config.name.clone(), config.command.clone(), args, config.kind, None);
            c.set_repo_root(repo_root.to_path_buf());
            Box::new(c)
        }
        TransportKind::Stdio => Box::new(StdioAdapter::new(config.name.clone(), config.command.clone(), args)),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(config.name.clone(), config.command.clone(), args)),
        TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(config.name.clone(), config.command.clone(), args)),
    }
}
```

Then in `notebook_run_context_with_runner`: build the connection, wrap `Arc::new(Mutex::new(connection))` into `AcpAgentBackend::new(conn, notebook_path)`, and pass `NotebookCellRunner::new(RunCellCommandRunner::new(deps), Arc::new(backend))` to `ReactiveEngine::new`. Thread the `AgentConfig` in as a new parameter (caller — the Tauri command — supplies the notebook's configured agent; default to the existing brain/agent config the app already loads). If the app has no obvious `AgentConfig` source, emit `risk` and stop rather than hardcoding a command.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-notebook run_context -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/dag/run_context.rs
git commit -m "feat(notebook): wire AcpAgentBackend into notebook run context"
```

**Scope Drift Checkpoint:**
- If the `AgentConfig` source is not discoverable in the notebook/app layer, emit `risk` (do not hardcode an agent command).
- If composing the runner forces signature changes across `engine.rs`/`cell_runner.rs`, emit `scope_drift`.

---

## Self-Review

- **Spec coverage:** §3.1→T1, §3.2→T2, §3.3+§5 data flow→T4, §3.4 arch→T4/T6 compose it, §4 cell model→T4 (kernelspec)+T5 (metadata), §5 port render→T3, §6 policy→T4 (cache) + T5 (stale/live), §7 errors→`AiError`(T1)+`From<AiError>`(T4), §8 testing→each task's TDD steps, §9 Tier-2 hook→T1 trait boundary, §11 open input→resolved in T6 reference. No uncovered requirement.
- **Placeholders:** code is concrete; remaining "read the real type and match accessors" notes are bounded by file:line references and guarded by `risk`/`scope_drift` signals — not silent TODOs.
- **Type consistency:** `AiRunRequest`/`AiRunOutput`/`PortContext`/`AiError` defined in T1 are used unchanged in T2/T3/T4; `NotebookCellRunner::new(inner, ai)` signature matches T6 usage.
- **DAG:** acyclic (T1→{T2,T3}; T3→T4→T5; {T2,T4}→T6). T4 and T5 serialized because both touch the engine cascade area / are adjacent; T2 and T3 parallel.
- **beads compat:** every task has a unique id, explicit `depends_on`, brain-verifiable acceptance criteria, and a scope boundary.
