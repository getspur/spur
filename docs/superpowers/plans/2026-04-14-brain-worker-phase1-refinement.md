# Brain-Worker Phase 1 Refinement — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enrich the brain ↔ worker communication pipe so the brain can make good planning decisions: thread `brain_session_id` for lineage, make `DelegationResult` decision-grade (widened, UTF-8-safe, tail-weighted summary + `DiffSummary`), and close the inner retry Reflexion loop.

**Architecture:** Additive changes across four existing files. Four private helpers (`truncate_summary`, `build_diff_summary`, `render_retry_context`, `apply_bloat_cap`) land in `orchestrator.rs`; struct changes on `DelegationRequest` (spur-mcp) and `DelegationResult` (spur-acp) are non-breaking `Option` additions. No new channels, no new crates, no protocol changes.

**Tech Stack:** Rust 1.88 workspace · tokio (async runtime) · serde/serde_json · tempfile (test fixtures) · git (CLI, via `tokio::process`).

**Spec:** `docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md`

---

## File Structure

Touched files only — no new files are created.

| File | Role after this change |
|---|---|
| `crates/spur-mcp/src/tools.rs` | `DelegationRequest` gains `brain_session_id: SessionId` |
| `crates/spur-mcp/src/server.rs` | `McpCallbackServer` persists `brain_session_id`; 8 handlers stamp it onto `DelegationRequest` |
| `crates/spur-acp/src/domain/delegation.rs` | `DelegationResult` gains `diff_summary: Option<DiffSummary>` |
| `crates/spur-acp/src/domain/events.rs` | Doc comments on `DelegationRequested.from` and `DelegationDispatched.from` updated (the existing "currently populated with worker session" caveats get removed) |
| `crates/spur-core/src/orchestrator.rs` | Adds four private helpers; threads `brain_session_id` through `execute_delegation` → `run_one_worker_attempt`; switches summary truncation to `truncate_summary`; replaces generic error string with output tail; populates `DelegationResult.diff_summary` and `ReviewPayload.diff_summary`; accumulates retry history |

---

## Task 1: `truncate_summary` helper

**Purpose:** UTF-8-safe, tail-weighted, env-configurable text truncation. Pure function. Replaces the unsafe byte-slice at `orchestrator.rs:2491-2497`.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (add private helper + inline unit tests)

- [ ] **Step 1.1: Write the failing unit tests**

Append this inline test module near the other helper test modules at the bottom of `crates/spur-core/src/orchestrator.rs`:

```rust
#[cfg(test)]
mod truncate_summary_tests {
    use super::truncate_summary;

    #[test]
    fn under_cap_returns_unchanged() {
        let input = "short text";
        assert_eq!(truncate_summary(input, 4000), "short text");
    }

    #[test]
    fn exact_cap_returns_unchanged() {
        let input = "x".repeat(100);
        assert_eq!(truncate_summary(&input, 100), input);
    }

    #[test]
    fn over_cap_preserves_head_and_tail_with_marker() {
        let input: String = (0..5000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let cap = 4000;
        let out = truncate_summary(&input, cap);
        assert!(out.len() < input.len(), "output must be shorter than input");
        assert!(out.contains("chars omitted"), "omission marker must appear");
        // Tail is 3/4 of cap → tail slice starts at input.len() - 3000.
        let tail_start = input.len() - 3000;
        assert!(
            out.ends_with(&input[tail_start..]),
            "output must end with the last 3000 chars of input"
        );
        // Head is 1/4 of cap → first 1000 chars.
        assert!(
            out.starts_with(&input[..1000]),
            "output must start with the first 1000 chars of input"
        );
    }

    #[test]
    fn utf8_boundary_does_not_panic() {
        // Each em-dash is 3 bytes. With cap=10, the naive cut at
        // byte 10 would land inside the 4th em-dash (bytes 9-11).
        let input = "—".repeat(20); // 60 bytes total
        let out = truncate_summary(&input, 10);
        // Must not panic. Must produce valid UTF-8 (implicit: String).
        assert!(out.chars().count() > 0);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(truncate_summary("", 4000), "");
    }

    #[test]
    fn env_var_overrides_default_cap() {
        // SAFETY: tests in this module are serialized by the test
        // harness when they touch env vars; set + restore.
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", "50") };
        let input = "x".repeat(200);
        let out = super::truncate_summary_env_default(&input);
        assert!(out.len() < input.len());
        assert!(out.len() <= 100, "output must respect env override, got {}", out.len());
        match prev {
            Some(v) => unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) },
            None => unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") },
        }
    }
}
```

- [ ] **Step 1.2: Run tests, confirm they fail**

```bash
cargo test -p spur-core --lib truncate_summary_tests 2>&1 | tail -30
```

Expected: compile error — `truncate_summary` and `truncate_summary_env_default` undefined.

- [ ] **Step 1.3: Implement both helpers**

Add these helpers near the other file-scope private helpers (alongside `shellexpand_tilde` / `dirs_home` around line 2517):

```rust
/// Tail-weighted, UTF-8-safe truncation for worker summaries.
///
/// Why tail-weighted: LLM worker output opens with task restatement
/// and closes with a crisp conclusion + file list. The middle holds
/// verbose tool-call transcripts with low decision-density. Brain-
/// relevant information is concentrated at the tail.
///
/// Returns `text` unchanged if `text.len() <= cap`. Otherwise keeps
/// `cap/4` head bytes and `cap - cap/4` tail bytes (both aligned to
/// char boundaries), joined by an omission marker.
fn truncate_summary(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let head_budget = cap / 4;
    let tail_budget = cap - head_budget;

    // str::floor_char_boundary / ceil_char_boundary are stable since 1.80.
    let head_end = text.floor_char_boundary(head_budget.min(text.len()));
    let tail_start = text.ceil_char_boundary(text.len().saturating_sub(tail_budget));

    // Clamp degenerate case where head and tail would overlap.
    let tail_start = tail_start.max(head_end);

    let omitted = tail_start - head_end;
    format!(
        "{}\n\n[... {} chars omitted ...]\n\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

/// Reads `SPUR_SUMMARY_MAX_BYTES` (default 4000) and applies `truncate_summary`.
fn truncate_summary_env_default(text: &str) -> String {
    let cap: usize = std::env::var("SPUR_SUMMARY_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000);
    truncate_summary(text, cap)
}
```

- [ ] **Step 1.4: Run tests, confirm they pass**

```bash
cargo test -p spur-core --lib truncate_summary_tests 2>&1 | tail -15
```

Expected: all 6 tests pass.

- [ ] **Step 1.5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): tail-weighted UTF-8-safe truncate_summary helper

Replaces the unsafe byte-slice at run_one_worker_attempt:2491 (not
yet wired — that's Task 6). Pure fn + env override via
SPUR_SUMMARY_MAX_BYTES, default 4000 bytes. Tail-weight = 1:3 because
LLM worker output concentrates decision-relevant content at the tail.
"
```

---

## Task 2: `build_diff_summary` helper

**Purpose:** Compute `DiffSummary { files_changed, insertions, deletions, files }` from a worktree by calling `git diff --numstat`. Replaces the brittle regex parser proposed in the architecture doc.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (add async helper + inline unit tests)

- [ ] **Step 2.1: Write the failing unit tests**

Append this inline test module (sibling of `truncate_summary_tests`):

```rust
#[cfg(test)]
mod build_diff_summary_tests {
    use super::build_diff_summary;
    use spur_acp::DiffSummary;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let path = dir.path();
        Command::new("git").arg("init").current_dir(path).output().unwrap();
        Command::new("git").args(["config", "user.email", "t@t"]).current_dir(path).output().unwrap();
        Command::new("git").args(["config", "user.name", "t"]).current_dir(path).output().unwrap();
        std::fs::write(path.join("a.txt"), "hello\nworld\n").unwrap();
        Command::new("git").args(["add", "."]).current_dir(path).output().unwrap();
        Command::new("git").args(["commit", "-m", "init"]).current_dir(path).output().unwrap();
        dir
    }

    #[tokio::test]
    async fn clean_worktree_returns_zero_summary() {
        let dir = init_repo();
        let summary = build_diff_summary(dir.path()).await.unwrap();
        assert_eq!(summary.files_changed, 0);
        assert_eq!(summary.insertions, 0);
        assert_eq!(summary.deletions, 0);
        assert!(summary.files.is_empty());
    }

    #[tokio::test]
    async fn modified_file_produces_expected_stats() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\nnew line\n").unwrap();
        let summary = build_diff_summary(dir.path()).await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 1);
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("a.txt")]);
    }

    #[tokio::test]
    async fn binary_file_is_counted_but_numbers_stay_zero() {
        let dir = init_repo();
        // numstat emits "-\t-\tpath" for binary files.
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 3, 0xFF]).unwrap();
        Command::new("git").args(["add", "b.bin"]).current_dir(dir.path()).output().unwrap();
        Command::new("git").args(["commit", "-m", "bin"]).current_dir(dir.path()).output().unwrap();
        std::fs::write(dir.path().join("b.bin"), [9u8, 8, 7]).unwrap();
        let summary = build_diff_summary(dir.path()).await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 0, "binary diff reports '-' for line counts");
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("b.bin")]);
    }
}
```

- [ ] **Step 2.2: Run tests, confirm they fail**

```bash
cargo test -p spur-core --lib build_diff_summary_tests 2>&1 | tail -30
```

Expected: compile error — `build_diff_summary` undefined.

- [ ] **Step 2.3: Implement the helper**

Add near `truncate_summary`:

```rust
/// Compute a `DiffSummary` for a worktree via `git diff --numstat`.
///
/// Preferred over regex-parsing the unified diff text because numstat
/// emits tab-separated stats directly and handles binary files (`-\t-\tpath`),
/// renames, and mode-only changes without ambiguity.
///
/// Cost: ~10-100ms. Same budget as `collect_diff`.
async fn build_diff_summary(worktree_path: &std::path::Path) -> anyhow::Result<spur_acp::DiffSummary> {
    use tokio::process::Command;

    let output = Command::new("git")
        .arg("diff")
        .arg("--numstat")
        .current_dir(worktree_path)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files_changed = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    let mut files = Vec::new();

    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("");
        let del = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        files_changed += 1;
        // numstat emits "-" for binary files. Non-"-" values parse as usize.
        insertions += ins.parse::<usize>().unwrap_or(0);
        deletions += del.parse::<usize>().unwrap_or(0);
        files.push(std::path::PathBuf::from(path));
    }

    Ok(spur_acp::DiffSummary {
        files_changed,
        insertions,
        deletions,
        files,
    })
}
```

- [ ] **Step 2.4: Run tests, confirm they pass**

```bash
cargo test -p spur-core --lib build_diff_summary_tests 2>&1 | tail -15
```

Expected: all 3 tests pass.

- [ ] **Step 2.5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): build_diff_summary helper using git diff --numstat

Produces DiffSummary from a worktree path. Handles binary files and
renames correctly via --numstat's tab-separated output. Not yet wired
— Task 5 populates DelegationResult.diff_summary and
ReviewPayload.diff_summary with this helper.
"
```

---

## Task 3: Thread `brain_session_id` through spur-mcp

**Purpose:** Add `brain_session_id` to `DelegationRequest`; persist it on `McpCallbackServer`; stamp it in all 8 handlers.

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` (add field to struct)
- Modify: `crates/spur-mcp/src/server.rs` (add field to server, update constructor, 8 handlers)

- [ ] **Step 3.1: Add `brain_session_id` field to `DelegationRequest`**

In `crates/spur-mcp/src/tools.rs:14-21`, change:

```rust
#[derive(Debug)]
pub struct DelegationRequest {
    pub id: String,
    pub agent: String,
    pub task: String,
    pub context_files: Vec<String>,
    /// Oneshot channel for the orchestrator to send the result back.
    pub respond_to: oneshot::Sender<DelegationResult>,
}
```

to:

```rust
#[derive(Debug)]
pub struct DelegationRequest {
    pub id: String,
    pub agent: String,
    pub task: String,
    pub context_files: Vec<String>,
    /// Oneshot channel for the orchestrator to send the result back.
    pub respond_to: oneshot::Sender<DelegationResult>,
    /// Brain session that originated this request. Threaded through so
    /// `DelegationRequested.from` / `DelegationDispatched.from` can
    /// correctly identify the brain in lineage. Stamped at every
    /// construction site in the MCP server.
    pub brain_session_id: spur_acp::SessionId,
}
```

- [ ] **Step 3.2: Add `brain_session_id` field to `McpCallbackServer`, persist in constructor**

In `crates/spur-mcp/src/server.rs:97-128`, change:

```rust
pub struct McpCallbackServer {
    socket_path: PathBuf,
    /// Channel to send delegation requests to the orchestrator.
    delegation_tx: mpsc::Sender<DelegationRequest>,
    /// Available worker agents (set once at creation).
    workers: Vec<WorkerInfo>,
}

impl McpCallbackServer {
    /// Create a new MCP callback server for the given session.
    ///
    /// Returns the server instance and a `DelegationChannel` that the
    /// orchestrator uses to receive requests and send responses.
    pub fn new(session_id: &SessionId) -> (Self, DelegationChannel) {
        let socket_path = PathBuf::from(format!("/tmp/spur-mcp-{session_id}.sock"));

        // Server -> Orchestrator: delegation requests (each request carries
        // its own oneshot sender for the response).
        let (req_tx, req_rx) = mpsc::channel::<DelegationRequest>(32);

        let server = Self {
            socket_path,
            delegation_tx: req_tx,
            workers: Vec::new(),
        };

        let channel = DelegationChannel {
            request_rx: req_rx,
        };

        (server, channel)
    }
```

to:

```rust
pub struct McpCallbackServer {
    socket_path: PathBuf,
    /// Channel to send delegation requests to the orchestrator.
    delegation_tx: mpsc::Sender<DelegationRequest>,
    /// Available worker agents (set once at creation).
    workers: Vec<WorkerInfo>,
    /// Brain session this server belongs to. Stamped onto every
    /// `DelegationRequest` so downstream events can attribute the
    /// request to the originating brain (not the worker session).
    brain_session_id: SessionId,
}

impl McpCallbackServer {
    /// Create a new MCP callback server for the given session.
    ///
    /// Returns the server instance and a `DelegationChannel` that the
    /// orchestrator uses to receive requests and send responses.
    pub fn new(session_id: &SessionId) -> (Self, DelegationChannel) {
        let socket_path = PathBuf::from(format!("/tmp/spur-mcp-{session_id}.sock"));

        // Server -> Orchestrator: delegation requests (each request carries
        // its own oneshot sender for the response).
        let (req_tx, req_rx) = mpsc::channel::<DelegationRequest>(32);

        let server = Self {
            socket_path,
            delegation_tx: req_tx,
            workers: Vec::new(),
            brain_session_id: session_id.clone(),
        };

        let channel = DelegationChannel {
            request_rx: req_rx,
        };

        (server, channel)
    }
```

- [ ] **Step 3.3: Stamp `brain_session_id` in all 8 handlers**

Eight `DelegationRequest { ... }` literals exist in `server.rs`. For each, add `brain_session_id: self.brain_session_id.clone(),` as the last field:

1. `handle_delegate_to_worker` — around line 326
2. `handle_delegate_parallel` — around line 407 (inside per-task loop)
3. `handle_get_issue` — around line 483
4. `handle_update_issue` — around line 531
5. `handle_create_pr` — around line 584
6. `handle_report_progress` — around line 631
7. `handle_get_session_cost` — around line 656

(That's 7 construction sites for 8 handlers; `list_available_workers` doesn't construct a `DelegationRequest`.)

Example — `handle_delegate_to_worker`:

```rust
let delegation = DelegationRequest {
    id: request_id.clone(),
    agent: agent.clone(),
    task: task.clone(),
    context_files,
    respond_to: tx,
    brain_session_id: self.brain_session_id.clone(),
};
```

Repeat the pattern at all 7 sites.

- [ ] **Step 3.4: Run workspace build, confirm everything compiles**

```bash
cargo build -p spur-mcp -p spur-core 2>&1 | tail -30
```

Expected: clean build OR errors in `spur-core` about missing field on the destructure in `handle_delegations` (that's Task 4).

- [ ] **Step 3.5: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): thread brain_session_id through DelegationRequest

McpCallbackServer persists the brain session id received at
construction and stamps it on every DelegationRequest (all 8 tool
handlers). Orchestrator consumption comes in the next commit.
"
```

---

## Task 4: Consume `brain_session_id` in orchestrator emissions

**Purpose:** Thread `brain_session_id` through `handle_delegations` → `execute_delegation` → `run_one_worker_attempt`; use in `DelegationRequested.from` and `DelegationDispatched.from`; remove the "currently populated with the worker session" caveats from event doc-comments.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (three function signatures + two event emissions)
- Modify: `crates/spur-acp/src/domain/events.rs` (doc comments on `DelegationRequested.from` and `DelegationDispatched.from`)

- [ ] **Step 4.1: Destructure `brain_session_id` in `handle_delegations`**

At `orchestrator.rs:1531-1537`, add the new field:

```rust
let DelegationRequest {
    id: request_id,
    agent,
    task,
    context_files,
    respond_to,
    brain_session_id,
} = request;
```

- [ ] **Step 4.2: Pass `brain_session_id` to the spawned task and into `execute_delegation`**

Update the `tokio::spawn(async move { ... })` block to move `brain_session_id` in, and update the `execute_delegation(...)` call to pass it as a new argument. Read the existing call site (should be inside the spawn block after the permit acquisition) and add `brain_session_id.clone()` or `brain_session_id` as an argument.

- [ ] **Step 4.3: Add `brain_session_id` parameter to `execute_delegation`**

At `orchestrator.rs:1629-1638`, change signature:

```rust
async fn execute_delegation(
    agent: String,
    original_task: String,
    _context_files: Vec<String>,
    request_id: String,
    brain_session_id: SessionId,           // NEW
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    event_tx: broadcast::Sender<SpurEvent>,
    review_sink: ReviewSink,
) -> (DelegationResult, Option<ExecutorId>) {
```

- [ ] **Step 4.4: Add `brain_session_id` parameter to `run_one_worker_attempt`**

At `orchestrator.rs:2357-2365`, change signature:

```rust
async fn run_one_worker_attempt(
    worker_session: SessionId,
    brain_session_id: &SessionId,          // NEW
    agent: &str,
    task: &str,
    request_id: &str,
    agent_config: &spur_acp::config::AgentConfig,
    worktrees: &mut WorktreeManager,
    event_tx: &broadcast::Sender<SpurEvent>,
) -> Result<WorkerAttemptOutcome, AttemptSetupError> {
```

Pass `&brain_session_id` at every call site in `execute_delegation` (there is one, inside the `loop { ... }`).

- [ ] **Step 4.5: Use `brain_session_id` in `DelegationRequested` emission**

At `orchestrator.rs:2373-2377`, change:

```rust
let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationRequested {
    from: worker_session.clone(),
    to_agent: agent.to_string(),
    task: task.to_string(),
    request_id: request_id.to_string(),
}));
```

to:

```rust
let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationRequested {
    from: brain_session_id.clone(),
    to_agent: agent.to_string(),
    task: task.to_string(),
    request_id: request_id.to_string(),
}));
```

Also drop the stale `NOTE:` comment block at lines 2366-2372 — its premise ("populated per-attempt with worker_session") is no longer true. Leave the adapter-keying caveat if it still applies; re-read the paragraph and keep only the still-accurate portion.

- [ ] **Step 4.6: Use `brain_session_id` in `DelegationDispatched` emission**

At `orchestrator.rs:2415-2419` (approximate — grep for `DelegationDispatched` to find it), change `from: worker_session.clone()` to `from: brain_session_id.clone()`.

- [ ] **Step 4.7: Update `events.rs` doc comments**

In `crates/spur-acp/src/domain/events.rs`, remove the "pre-existing limitation" caveats:

At lines 130-142 (`DelegationRequested`), change:

```rust
DelegationRequested {
    /// **Currently populated with the worker session**, not the brain session —
    /// pre-existing limitation: the brain session id is not threaded into
    /// the orchestrator. To be corrected alongside `DelegationDispatched.from`
    /// in the follow-up task that wires the brain session through.
    from: SessionId,
    to_agent: String,
    task: String,
    /// UUID matching the spur-mcp `DelegationRequest.id`. Surfaced so
    /// the brain conversation can correlate with the spawned executor
    /// via `DelegationDispatched`.
    request_id: String,
},
```

to:

```rust
DelegationRequested {
    /// Brain session that issued the delegation. Stamped by the MCP
    /// server onto every `DelegationRequest` and threaded through the
    /// orchestrator to this emission site.
    from: SessionId,
    to_agent: String,
    task: String,
    /// UUID matching the spur-mcp `DelegationRequest.id`. Surfaced so
    /// the brain conversation can correlate with the spawned executor
    /// via `DelegationDispatched`.
    request_id: String,
},
```

At lines 147-159 (`DelegationDispatched`), similarly replace the caveat on `from` with the positive description.

- [ ] **Step 4.8: Build the workspace**

```bash
cargo build --workspace 2>&1 | tail -30
```

Expected: clean build.

- [ ] **Step 4.9: Grep-verify no stale `from: worker_session` emissions remain**

```bash
rg 'DelegationRequested|DelegationDispatched' crates/spur-core/src/orchestrator.rs -A 3
```

Expected output: both emissions have `from: brain_session_id.clone()` (or similar). No occurrence of `from: worker_session`.

- [ ] **Step 4.10: Run the full test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all existing tests still pass. Type system enforces correct threading.

- [ ] **Step 4.11: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-acp/src/domain/events.rs
git commit -m "feat(spur-core): consume brain_session_id in delegation events

DelegationRequested.from and DelegationDispatched.from now carry the
brain session id (was worker_session). Updates event doc-comments to
remove the 'pre-existing limitation' caveats and describe the positive
invariant.

Closes the lineage-tagging half of the brain-worker Phase 1 refinement.
"
```

---

## Task 5: Enrich `DelegationResult` with `diff_summary` + populate both sites

**Purpose:** Add `diff_summary: Option<DiffSummary>` to `DelegationResult`; populate it at `run_one_worker_attempt` via `build_diff_summary`; populate `ReviewPayload.diff_summary` at the review gate (currently `None` at `orchestrator.rs:1814`).

**Files:**
- Modify: `crates/spur-acp/src/domain/delegation.rs` (struct change)
- Modify: `crates/spur-core/src/orchestrator.rs` (WorkerAttemptOutcome + wire helper to DelegationResult + ReviewPayload)

- [ ] **Step 5.1: Write the failing round-trip serde test**

Append to `crates/spur-acp/src/domain/delegation.rs`:

```rust
#[cfg(test)]
mod delegation_result_tests {
    use super::*;
    use crate::DiffSummary;
    use std::path::PathBuf;

    #[test]
    fn result_with_diff_summary_round_trips_json() {
        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: Some("--- a/x\n+++ b/x\n".into()),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 3,
                deletions: 1,
                files: vec![PathBuf::from("x")],
            }),
            summary: Some("did the thing".into()),
            estimated_cost_usd: 0.42,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: DelegationResult = serde_json::from_str(&json).unwrap();
        let ds = back.diff_summary.expect("diff_summary should round-trip");
        assert_eq!(ds.files_changed, 1);
        assert_eq!(ds.insertions, 3);
        assert_eq!(ds.files, vec![PathBuf::from("x")]);
    }

    #[test]
    fn result_without_diff_summary_deserializes_old_payloads() {
        // Older payloads omit the field entirely. serde must accept.
        let json = r#"{"status":"Success","diff":null,"summary":null,"estimated_cost_usd":0.0}"#;
        let back: DelegationResult = serde_json::from_str(json).unwrap();
        assert!(back.diff_summary.is_none());
    }
}
```

- [ ] **Step 5.2: Run test, confirm failure**

```bash
cargo test -p spur-acp delegation_result_tests 2>&1 | tail -15
```

Expected: compile error — `diff_summary` field doesn't exist.

- [ ] **Step 5.3: Add `diff_summary` field to `DelegationResult`**

In `crates/spur-acp/src/domain/delegation.rs:53-59`:

```rust
/// Result returned from a completed delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub status: DelegationStatus,
    pub diff: Option<String>,
    /// Structured diff stats (files changed, lines added/removed, file list).
    /// Populated from `git diff --numstat` at result-construction time.
    /// `None` when the worker produced no diff (setup failure, empty diff,
    /// or the diff call failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<DiffSummary>,
    pub summary: Option<String>,
    pub estimated_cost_usd: f64,
}
```

Make sure `use crate::DiffSummary;` is present at the top of the file (the type lives in `crate::domain::events` and is re-exported from the crate root).

- [ ] **Step 5.4: Run test, confirm pass**

```bash
cargo test -p spur-acp delegation_result_tests 2>&1 | tail -10
```

Expected: both tests pass.

- [ ] **Step 5.5: Add `diff_summary` field to `WorkerAttemptOutcome`**

In `crates/spur-core/src/orchestrator.rs:2251-2262`:

```rust
struct WorkerAttemptOutcome {
    worker_session: SessionId,
    candidate_status: DelegationStatus,
    diff: Option<String>,
    diff_summary: Option<spur_acp::DiffSummary>,   // NEW
    summary: Option<String>,
    cost: f64,
    worktree_path: PathBuf,
}
```

- [ ] **Step 5.6: Populate `diff_summary` at the end of `run_one_worker_attempt`**

At `orchestrator.rs:2474-2515` (the block that builds the outcome), right after `let diff = ...` (currently line 2474-2478), add:

```rust
// Compute structured diff stats alongside the raw diff text.
// `None` if numstat errors — non-fatal, we still return what we have.
let diff_summary = match build_diff_summary(&worktree_path).await {
    Ok(s) if s.files_changed > 0 => Some(s),
    _ => None,
};
```

Note: `worktree_path` is computed a few lines below (line 2482). Either move the `diff_summary` computation after `worktree_path` is set, or compute `worktree_path` first. Choose the order that keeps the diff minimal.

Then add `diff_summary,` to the `WorkerAttemptOutcome { ... }` literal at line 2507.

- [ ] **Step 5.7: Thread `diff_summary` from outcome into `DelegationResult`**

`finalize` at `orchestrator.rs:2201` is the single site that constructs the final `DelegationResult`. Update `finalize`'s signature to accept `diff_summary: Option<DiffSummary>` and populate the new field on the result. Update every call site of `finalize` to pass `outcome.diff_summary.clone()` (or `None` for setup-failure call sites that don't have a worker outcome).

Read `finalize` first (around line 2201) to see its current parameter list; add the new parameter just after `diff`. Example target shape:

```rust
fn finalize(
    event_tx: &broadcast::Sender<SpurEvent>,
    worker_session: SessionId,
    status: DelegationStatus,
    diff: Option<String>,
    diff_summary: Option<spur_acp::DiffSummary>,   // NEW
    summary: Option<String>,
    total_cost: f64,
) -> DelegationResult { ... }
```

- [ ] **Step 5.8: Populate `ReviewPayload.diff_summary` at the review gate**

At `orchestrator.rs:1812-1817`:

```rust
let review_payload = ReviewPayload {
    summary: outcome.summary.clone().unwrap_or_default(),
    diff_summary: None,
    pr_url: None,
    error: None,
};
```

Change to:

```rust
let review_payload = ReviewPayload {
    summary: outcome.summary.clone().unwrap_or_default(),
    diff_summary: outcome.diff_summary.clone(),
    pr_url: None,
    error: None,
};
```

- [ ] **Step 5.9: Build the workspace**

```bash
cargo build --workspace 2>&1 | tail -20
```

Expected: clean build.

- [ ] **Step 5.10: Run tests**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all tests pass (existing + new delegation_result_tests).

- [ ] **Step 5.11: Commit**

```bash
git add crates/spur-acp/src/domain/delegation.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-acp,spur-core): populate DelegationResult.diff_summary

Adds diff_summary: Option<DiffSummary> to DelegationResult and
populates it at run_one_worker_attempt via build_diff_summary. Also
fills in ReviewPayload.diff_summary (previously None) at the review
gate, so human reviewers and the brain see symmetric diff stats.
"
```

---

## Task 6: Swap in `truncate_summary` + replace generic error string

**Purpose:** Replace the unsafe byte-slice summary truncation (`&output_text[..500]`) with `truncate_summary_env_default`. Replace the literal `"Worker reported errors"` with the tail of `output_text` for concrete error signal.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (run_one_worker_attempt lines 2491-2505)

- [ ] **Step 6.1: Write the failing regression test for UTF-8 safety**

Append to the inline test modules:

```rust
#[cfg(test)]
mod summary_wiring_tests {
    use super::*;

    #[test]
    fn truncate_summary_env_default_handles_utf8_at_500() {
        // Build a string where a multi-byte char straddles byte 500.
        // Em-dash = 3 bytes. "a" = 1 byte. 498 'a's + 1 em-dash = 501 bytes.
        let mut s = String::from("a".repeat(498));
        s.push('—'); // bytes 498, 499, 500
        s.push_str(&"b".repeat(5000));
        // Must not panic. Output must be valid UTF-8 (implicit: String return).
        let out = truncate_summary_env_default(&s);
        // Every byte index up to and including len must be a char boundary
        // on any valid String, but assert at len explicitly to catch
        // accidental construction via `from_utf8_unchecked`.
        assert!(out.is_char_boundary(out.len()));
        assert!(!out.is_empty());
    }
}
```

- [ ] **Step 6.2: Run test, confirm it does NOT panic (it should pass even before the swap since `truncate_summary_env_default` already exists from Task 1)**

```bash
cargo test -p spur-core --lib summary_wiring_tests 2>&1 | tail -15
```

Expected: pass. (This test validates the helper works independently; the next step wires it in.)

- [ ] **Step 6.3: Replace byte-slice summary truncation**

At `orchestrator.rs:2491-2497`, change:

```rust
let summary = if output_text.len() > 500 {
    Some(format!("{}...", &output_text[..500]))
} else if output_text.is_empty() {
    None
} else {
    Some(output_text)
};
```

to:

```rust
let summary = if output_text.is_empty() {
    None
} else {
    Some(truncate_summary_env_default(&output_text))
};
```

Note: `output_text` is consumed in the final branch; if `truncate_summary_env_default` takes `&str`, the above works. If lifetime issues arise, pass a clone.

- [ ] **Step 6.4: Replace generic error string with output tail**

At `orchestrator.rs:2499-2505`, change:

```rust
let candidate_status = if worker_success {
    DelegationStatus::Success
} else {
    DelegationStatus::Failed {
        error: "Worker reported errors".into(),
    }
};
```

to:

```rust
let candidate_status = if worker_success {
    DelegationStatus::Success
} else {
    // Capture the last ~500 chars of output as the error message.
    // For LLM/tool workers this is almost always the actual failure
    // (compiler error, test assertion, panic). Char-boundary-safe
    // via truncate_summary's tail path. `summary` is already widened
    // and kept separately — the error field here is the *signal*.
    let error = summary
        .as_deref()
        .map(|s| {
            let tail_len = 500usize.min(s.len());
            let start = s.ceil_char_boundary(s.len().saturating_sub(tail_len));
            s[start..].to_string()
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Worker reported errors (no output captured)".into());
    DelegationStatus::Failed { error }
};
```

- [ ] **Step 6.5: Build the workspace**

```bash
cargo build --workspace 2>&1 | tail -15
```

Expected: clean build.

- [ ] **Step 6.6: Run tests**

```bash
cargo test --workspace 2>&1 | tail -15
```

Expected: all tests pass.

- [ ] **Step 6.7: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): UTF-8-safe widened summary + error-tail capture

run_one_worker_attempt now uses truncate_summary_env_default (4000
bytes by default, SPUR_SUMMARY_MAX_BYTES override). The generic
'Worker reported errors' string is replaced with the last ~500 chars
of worker output — almost always the concrete failure signal the
brain needs for its next retry/abandon decision.

Fixes latent UTF-8 panic at '&output_text[..500]'.
"
```

---

## Task 7: `RetryAttempt` struct + `render_retry_context` + `apply_bloat_cap`

**Purpose:** Pure helpers for retry history. Module-local struct + two free functions. All unit-testable.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (add struct + two helpers + inline tests)

- [ ] **Step 7.1: Write the failing unit tests**

Append another inline test module:

```rust
#[cfg(test)]
mod retry_context_tests {
    use super::{apply_bloat_cap, render_retry_context, RetryAttempt};
    use spur_acp::DiffSummary;
    use std::path::PathBuf;

    fn att(n: u32, summary: &str, feedback: &str) -> RetryAttempt {
        RetryAttempt {
            attempt_n: n,
            summary: summary.into(),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 10,
                deletions: 2,
                files: vec![PathBuf::from("f.rs")],
            }),
            feedback: feedback.into(),
        }
    }

    #[test]
    fn render_includes_original_task_and_all_attempts_and_current_feedback() {
        let history = vec![
            att(1, "tried approach A", "needs tests"),
            att(2, "tried approach B", "still too slow"),
        ];
        let out = render_retry_context(&history, "make foo fast", "use async");
        assert!(out.contains("make foo fast"));
        assert!(out.contains("Attempt 1"));
        assert!(out.contains("tried approach A"));
        assert!(out.contains("needs tests"));
        assert!(out.contains("Attempt 2"));
        assert!(out.contains("tried approach B"));
        assert!(out.contains("still too slow"));
        assert!(out.contains("use async"));
        // Diff stats should render.
        assert!(out.contains("1 changed"));
        assert!(out.contains("+10"));
        assert!(out.contains("-2"));
    }

    #[test]
    fn render_handles_empty_history() {
        let out = render_retry_context(&[], "task", "feedback");
        assert!(out.contains("task"));
        assert!(out.contains("feedback"));
        // No attempt headers when history is empty.
        assert!(!out.contains("Attempt 1"));
    }

    #[test]
    fn apply_bloat_cap_drops_oldest_first() {
        // Build attempts with summaries of ~1000 bytes each. Cap at 2000.
        let big = "x".repeat(1000);
        let mut history = vec![
            att(1, &big, "fb1"),
            att(2, &big, "fb2"),
            att(3, &big, "fb3"),
        ];
        apply_bloat_cap(&mut history, 2000);
        // Should retain attempts 2 and 3 (newest), drop 1.
        assert!(history.iter().all(|a| a.attempt_n != 1));
        assert!(history.iter().any(|a| a.attempt_n == 3));
    }

    #[test]
    fn apply_bloat_cap_is_noop_when_under_cap() {
        let mut history = vec![att(1, "s", "f")];
        apply_bloat_cap(&mut history, 10_000);
        assert_eq!(history.len(), 1);
    }
}
```

- [ ] **Step 7.2: Run tests, confirm failure**

```bash
cargo test -p spur-core --lib retry_context_tests 2>&1 | tail -15
```

Expected: compile error — none of the names exist.

- [ ] **Step 7.3: Implement the struct and helpers**

Add near the other helpers (alongside `build_diff_summary`):

```rust
/// One retry attempt's surviving state, kept in memory across the
/// retry loop so later attempts can see the history. Module-local;
/// does not leak into public API.
#[derive(Debug, Clone)]
struct RetryAttempt {
    attempt_n: u32,
    summary: String,
    diff_summary: Option<spur_acp::DiffSummary>,
    /// Reviewer's `new_constraints` verbatim, the feedback that
    /// triggered this retry decision.
    feedback: String,
}

/// Render the augmented task prompt fed to the NEXT retry attempt.
///
/// Layout:
///   {original_task}
///
///   --- Previous attempts ---
///   Attempt N:
///     What was tried: {summary}
///     Files touched: {files_changed} changed, +{ins}/-{del}
///     Reviewer feedback: {feedback}
///   ...
///
///   --- Your task ---
///   Address the reviewer's most recent feedback above. Do NOT repeat
///   approaches that were rejected earlier — the reviewer sees the
///   same history and will reject a repeat.
///
///   Most recent feedback:
///   {current_feedback}
fn render_retry_context(
    history: &[RetryAttempt],
    original_task: &str,
    current_feedback: &str,
) -> String {
    let mut out = String::with_capacity(original_task.len() + current_feedback.len() + 512);
    out.push_str(original_task);

    if !history.is_empty() {
        out.push_str("\n\n--- Previous attempts ---\n");
        for a in history {
            out.push_str(&format!("\nAttempt {}:\n", a.attempt_n));
            out.push_str(&format!("  What was tried: {}\n", a.summary));
            if let Some(ds) = &a.diff_summary {
                out.push_str(&format!(
                    "  Files touched: {} changed, +{}/-{}\n",
                    ds.files_changed, ds.insertions, ds.deletions
                ));
            }
            out.push_str(&format!("  Reviewer feedback: {}\n", a.feedback));
        }
    }

    out.push_str(
        "\n--- Your task ---\n\
         Address the reviewer's most recent feedback above. Do NOT repeat \
         approaches that were rejected earlier — the reviewer sees the \
         same history and will reject a repeat.\n\n\
         Most recent feedback:\n",
    );
    out.push_str(current_feedback);
    out
}

/// Drop oldest attempts until the total in-memory summary+feedback
/// footprint fits under `max_bytes`. Preserves the most recent
/// attempts (those are most relevant to the current feedback).
fn apply_bloat_cap(history: &mut Vec<RetryAttempt>, max_bytes: usize) {
    fn size(a: &RetryAttempt) -> usize {
        a.summary.len() + a.feedback.len()
    }
    while history.iter().map(size).sum::<usize>() > max_bytes && !history.is_empty() {
        history.remove(0);
    }
}
```

- [ ] **Step 7.4: Run tests, confirm pass**

```bash
cargo test -p spur-core --lib retry_context_tests 2>&1 | tail -15
```

Expected: all 4 tests pass.

- [ ] **Step 7.5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): RetryAttempt + render_retry_context + apply_bloat_cap

Pure helpers for the retry Reflexion loop. Not yet wired (Task 8 does
the integration). 2KB bloat cap drops oldest attempts first so the
most recent feedback stays in-scope.
"
```

---

## Task 8: Integrate retry accumulator into `execute_delegation`

**Purpose:** Actually use the retry helpers. Replace the flat `current_task = format!("{}\n\n## Additional constraints\n{}", original_task, new_constraints)` with accumulated retry history rendered via `render_retry_context`.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (execute_delegation retry arm, lines 1694 retry-loop setup + 2019-2022 augmentation)
- Create: `crates/spur-core/tests/retry_reflexion.rs` (integration test)

- [ ] **Step 8.1: Write the failing integration test**

Create `crates/spur-core/tests/retry_reflexion.rs`:

```rust
//! Verifies that retry workers see the accumulated history of
//! previous attempts (summary, diff stats, reviewer feedback).
//!
//! Exercises render_retry_context directly — the integration path
//! from execute_delegation into the worker prompt is covered by
//! the inline unit tests in orchestrator.rs.

use spur_acp::DiffSummary;
use spur_core::orchestrator::test_support::{render_retry_context_public, RetryAttemptPublic};
use std::path::PathBuf;

#[test]
fn third_attempt_prompt_contains_both_prior_attempts() {
    let history = vec![
        RetryAttemptPublic {
            attempt_n: 1,
            summary: "I added rate limiting as a fixed window".into(),
            diff_summary: Some(DiffSummary {
                files_changed: 2,
                insertions: 40,
                deletions: 3,
                files: vec![PathBuf::from("src/rl.rs"), PathBuf::from("src/lib.rs")],
            }),
            feedback: "prefer token bucket".into(),
        },
        RetryAttemptPublic {
            attempt_n: 2,
            summary: "Switched to token bucket with hardcoded limits".into(),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 22,
                deletions: 8,
                files: vec![PathBuf::from("src/rl.rs")],
            }),
            feedback: "make the bucket size configurable per endpoint".into(),
        },
    ];
    let prompt = render_retry_context_public(
        &history,
        "Add rate limiting middleware",
        "make the bucket size configurable per endpoint",
    );

    // Both prior attempts' summaries must appear.
    assert!(prompt.contains("I added rate limiting as a fixed window"));
    assert!(prompt.contains("Switched to token bucket"));
    // Reviewer feedback from each attempt must appear.
    assert!(prompt.contains("prefer token bucket"));
    assert!(prompt.contains("configurable per endpoint"));
    // Diff stats render.
    assert!(prompt.contains("2 changed"));
    assert!(prompt.contains("+40"));
    // Original task is preserved.
    assert!(prompt.starts_with("Add rate limiting middleware"));
}
```

- [ ] **Step 8.2: Export test-support shims from orchestrator**

The integration test needs access to `RetryAttempt` and `render_retry_context`, but they're module-local. Add a test-support module in `crates/spur-core/src/orchestrator.rs` (or in `src/lib.rs`):

```rust
// In src/orchestrator.rs, near end of file but OUTSIDE #[cfg(test)] modules:

#[doc(hidden)]
pub mod test_support {
    //! Public shims for integration tests. Not part of the stable API.
    use spur_acp::DiffSummary;

    pub struct RetryAttemptPublic {
        pub attempt_n: u32,
        pub summary: String,
        pub diff_summary: Option<DiffSummary>,
        pub feedback: String,
    }

    pub fn render_retry_context_public(
        history: &[RetryAttemptPublic],
        original_task: &str,
        current_feedback: &str,
    ) -> String {
        let internal: Vec<super::RetryAttempt> = history
            .iter()
            .map(|a| super::RetryAttempt {
                attempt_n: a.attempt_n,
                summary: a.summary.clone(),
                diff_summary: a.diff_summary.clone(),
                feedback: a.feedback.clone(),
            })
            .collect();
        super::render_retry_context(&internal, original_task, current_feedback)
    }
}
```

Also re-export from `src/lib.rs`:

```rust
pub use orchestrator::test_support;   // near the other `pub use orchestrator::...` lines
```

- [ ] **Step 8.3: Run integration test, confirm failure**

```bash
cargo test -p spur-core --test retry_reflexion 2>&1 | tail -15
```

Expected: compile error — re-export or module missing. Should now compile after Step 8.2.

- [ ] **Step 8.4: Verify test passes (render is already implemented in Task 7)**

```bash
cargo test -p spur-core --test retry_reflexion 2>&1 | tail -10
```

Expected: test passes (helpers were built in Task 7).

- [ ] **Step 8.5: Wire the accumulator into `execute_delegation`**

At `orchestrator.rs:1673-1692`, just before the `loop {` (around line 1694), add:

```rust
let mut retry_history: Vec<RetryAttempt> = Vec::new();
```

At the retry arm — `orchestrator.rs:1951-2042` — locate the block that builds `current_task`. Currently:

```rust
// Append constraints to the ORIGINAL task (not
// the accumulated one — prevents compounding
// constraint text across N retries).
current_task = format!(
    "{}\n\n## Additional constraints\n{}",
    original_task, new_constraints
);
```

Replace with:

```rust
// Record this attempt in the retry history before re-prompting.
// See docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md
// for the rationale — inverts the original "prevent compounding"
// choice in favor of Reflexion, with a 2KB bloat cap as the
// mitigation.
retry_history.push(RetryAttempt {
    attempt_n,
    summary: outcome.summary.clone().unwrap_or_default(),
    diff_summary: outcome.diff_summary.clone(),
    feedback: new_constraints.clone(),
});
apply_bloat_cap(&mut retry_history, 2048);

current_task = render_retry_context(
    &retry_history,
    &original_task,
    &new_constraints,
);
```

Note: the existing comment at lines 2016-2018 about "prevents compounding constraint text" should be replaced or removed — the new code intentionally does compound, with a bloat cap mitigating prompt growth.

- [ ] **Step 8.6: Build the workspace**

```bash
cargo build --workspace 2>&1 | tail -15
```

Expected: clean build.

- [ ] **Step 8.7: Run full test suite**

```bash
cargo test --workspace 2>&1 | tail -25
```

Expected: all tests pass (integration + unit + existing).

- [ ] **Step 8.8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/src/lib.rs crates/spur-core/tests/retry_reflexion.rs
git commit -m "feat(spur-core): integrate retry accumulator in execute_delegation

Each retry attempt now sees the summaries, diff stats, and reviewer
feedback from ALL prior attempts (capped at 2KB total — drops oldest
first). Inverts the prior 'prevent compounding' choice in favor of
the Reflexion pattern that every surveyed framework implements
(Anthropic, LangGraph, CrewAI, Stevens/VIGIL).

Closes the inner retry feedback loop — the brain-level history loop
remains Phase 2.
"
```

---

## Self-Review Checklist (for the implementing agent)

After Task 8 lands, verify:

- [ ] **Spec coverage:** Each Goal from the spec maps to a committed task.
  - Thread brain session identity → Task 3 + Task 4
  - Decision-grade DelegationResult → Tasks 5 + 6
  - Close retry Reflexion loop → Tasks 7 + 8
  - Reuse helper at both DelegationResult and ReviewPayload sites → Task 5

- [ ] **No placeholders:** `rg "TODO|TBD|FIXME" crates/spur-core/src/orchestrator.rs crates/spur-mcp crates/spur-acp/src/domain/` returns no new entries introduced by this plan.

- [ ] **Type consistency:** `cargo build --workspace` passes clean. `cargo clippy --workspace -- -D warnings` passes.

- [ ] **UTF-8 safety:** No remaining `&some_str[..N]` byte-slice in changed files. `rg '\[\.\.[0-9]+\]' crates/spur-core/src/orchestrator.rs crates/spur-mcp crates/spur-acp` returns no new matches.

- [ ] **Manual smoke:** Run `spur watch` with a brain. Have it delegate a small task with `review_required=true`. Reject with feedback. Verify the retry worker's prompt in the TUI includes the first attempt's summary.

---

## Out-of-scope (explicit — do NOT attempt in this plan)

- `ErrorKind` taxonomy enum — needs worker-side structured exit. Phase 2.
- `expected_output` field on `DelegationRequest` — own spec.
- Executor abstraction / worker cancellation tracking. Phase 2.
- Split broadcast bus. Phase 2.
- WORKER_REPORT.md filesystem handoff. Phase 2.
- Async delegation model (non-blocking `delegate_to_worker`). Protocol-level change; separate design cycle.
- Brain-level retry history (retry metadata surfaced in `DelegationResult`). Phase 2 after inner loop validates.
