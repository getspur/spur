# TUI Lineage Pane Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three layer-leakage bugs in the SPUR TUI Lineage pane: double-firing keyboard navigation, oldest-on-top ordering, and the elapsed timer continuing past terminal phases.

**Architecture:** Each fix re-establishes a layer boundary the L9 way:
- **F1 (double nav):** unify selection through counted Action variants (`SelectNextBy(usize)`/`SelectPrevBy(usize)`); the view never mutates selection inline.
- **F3 (frozen timer):** push elapsed-time semantics onto `Attempt` itself with an injectable `now`; all three render call sites converge on one helper.
- **F2 (newest on top):** collapse three independent tree-walkers in `agents_tree.rs` into one canonical `visible_order` that reverses once; reverse `pending_reviews` at the `JumpToReview` call site so visual + navigation directions stay aligned.

**Tech Stack:** Rust, ratatui, crossterm. Workspace crates: `spur-core` (lineage projection + types), `spur-tui` (dashboard + components).

**PR sequence (low → high review friction):**
1. **PR1 — F1 / A3** Counted-selection actions. Surgical, no snapshot churn.
2. **PR2 — F3 / C3** `Attempt::elapsed_at` helper, fix three call sites. Pure refactor + behavior change on terminal nodes only.
3. **PR3 — F2 / B2′** Collapse three walkers into one + reverse iteration + fix `JumpToReview`. Loud snapshot diff goes last.

---

## File Structure

### Files modified

- `crates/spur-tui/src/action.rs` — replace `Action::SelectNext`/`SelectPrev` with counted variants (PR1).
- `crates/spur-tui/src/views/dashboard.rs` — remove inline `agents_tree.select_*` calls; emit counted actions (PR1). Update elapsed rendering uses (PR2 indirectly).
- `crates/spur-tui/src/app.rs` — handle counted-selection actions; reverse `pending_reviews` in `JumpToReview` / `JumpToPreviousReview` (PR1 + PR3).
- `crates/spur-core/src/lineage/types.rs` — add `Attempt::elapsed_at(now: SystemTime) -> Duration`; rewrite `ExecutorNode::elapsed_secs()` to use it (PR2).
- `crates/spur-tui/src/components/agents_tree.rs` — replace inline elapsed math with `node.elapsed_secs()` (PR2); collapse `visible_order`/`render`/`render_lineage_to_strings` into one canonical traversal that reverses iteration (PR3).

### Files added

- `crates/spur-core/src/lineage/types.rs` (in-place test module) — unit tests for `Attempt::elapsed_at`.

### Files NOT modified (intentional)

- `crates/spur-core/src/lineage/projection.rs` — replay-purity invariant forbids changes to storage order or timestamps. We only consume what the projection already records.

---

## PR1 — F1 / A3: Counted-selection actions

### Task 1.1: Replace `SelectNext`/`SelectPrev` with counted variants in `action.rs`

**Files:**
- Modify: `crates/spur-tui/src/action.rs:97-100`

- [ ] **Step 1: Replace the action variants**

In `crates/spur-tui/src/action.rs`, replace lines 97-100:

```rust
    /// Move tree selection down one row.
    SelectNext,
    /// Move tree selection up one row.
    SelectPrev,
```

with:

```rust
    /// Move tree selection down by N rows.
    SelectNextBy(usize),
    /// Move tree selection up by N rows.
    SelectPrevBy(usize),
```

- [ ] **Step 2: Verify the file compiles in isolation**

Run: `cargo check -p spur-tui --message-format short 2>&1 | head -40`
Expected: compile errors at every `Action::SelectNext` / `Action::SelectPrev` call site (these will be fixed in Tasks 1.2 and 1.3).

### Task 1.2: Update action handlers in `app.rs`

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1996-2001`

- [ ] **Step 1: Replace the two single-step handlers with counted handlers**

In `crates/spur-tui/src/app.rs`, replace lines 1996-2001:

```rust
            Action::SelectNext => {
                self.dashboard.agents_tree_mut().select_next(&self.lineage);
            }
            Action::SelectPrev => {
                self.dashboard.agents_tree_mut().select_prev(&self.lineage);
            }
```

with:

```rust
            Action::SelectNextBy(n) => {
                for _ in 0..n {
                    self.dashboard.agents_tree_mut().select_next(&self.lineage);
                }
            }
            Action::SelectPrevBy(n) => {
                for _ in 0..n {
                    self.dashboard.agents_tree_mut().select_prev(&self.lineage);
                }
            }
```

- [ ] **Step 2: Verify**

Run: `cargo check -p spur-tui --message-format short 2>&1 | grep -E "SelectNext|SelectPrev" | head -20`
Expected: compile errors only inside `crates/spur-tui/src/views/dashboard.rs` at the call sites Task 1.3 will fix.

### Task 1.3: Remove inline `select_*` calls in `dashboard.rs`; emit counted actions

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs:1126-1149` (Vim Normal `j`/`k`)
- Modify: `crates/spur-tui/src/views/dashboard.rs:1242-1271` (Insert `j`/`k`)
- Modify: `crates/spur-tui/src/views/dashboard.rs:1322-1351` (Up/Down arrows)
- Modify: `crates/spur-tui/src/views/dashboard.rs:1353-1383` (PageUp/PageDown)

- [ ] **Step 1: Replace Vim Normal `j` arm (lines 1126-1133)**

Replace:

```rust
                        'j' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            if let Some(lineage) = lineage {
                                self.agents_tree.select_next(lineage);
                            }
                            Some(Action::SelectNext)
                        }
```

with:

```rust
                        'j' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            let _ = lineage;
                            Some(Action::SelectNextBy(1))
                        }
```

- [ ] **Step 2: Replace Vim Normal `k` arm (lines 1143-1150)**

Replace:

```rust
                        'k' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            if let Some(lineage) = lineage {
                                self.agents_tree.select_prev(lineage);
                            }
                            Some(Action::SelectPrev)
                        }
```

with:

```rust
                        'k' if self.focused_panel == Panel::Agents
                            && self.focused_node.is_none() =>
                        {
                            let _ = lineage;
                            Some(Action::SelectPrevBy(1))
                        }
```

- [ ] **Step 3: Replace Insert mode `j` arm (lines 1243-1248)**

Replace:

```rust
                    'j' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        if let Some(lineage) = lineage {
                            self.agents_tree.select_next(lineage);
                        }
                        Some(Action::SelectNext)
                    }
```

with:

```rust
                    'j' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        let _ = lineage;
                        Some(Action::SelectNextBy(1))
                    }
```

- [ ] **Step 4: Replace Insert mode `k` arm (lines 1258-1263)**

Replace:

```rust
                    'k' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        if let Some(lineage) = lineage {
                            self.agents_tree.select_prev(lineage);
                        }
                        Some(Action::SelectPrev)
                    }
```

with:

```rust
                    'k' if self.focused_panel == Panel::Agents && self.focused_node.is_none() => {
                        let _ = lineage;
                        Some(Action::SelectPrevBy(1))
                    }
```

- [ ] **Step 5: Replace `KeyCode::Up` arm (lines 1322-1336)**

Replace:

```rust
            KeyCode::Up => {
                if let Some(ref id) = self.focused_node.clone() {
                    let _trace = worker_streams.get_mut(&id.0);
                    self.detail_pane.scroll_up();
                    Some(Action::ScrollUp)
                } else if self.focused_panel == Panel::Agents {
                    if let Some(lineage) = lineage {
                        self.agents_tree.select_prev(lineage);
                    }
                    Some(Action::SelectPrev)
                } else {
                    self.activity_log.scroll_up();
                    Some(Action::ScrollUp)
                }
            }
```

with:

```rust
            KeyCode::Up => {
                if let Some(ref id) = self.focused_node.clone() {
                    let _trace = worker_streams.get_mut(&id.0);
                    self.detail_pane.scroll_up();
                    Some(Action::ScrollUp)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectPrevBy(1))
                } else {
                    self.activity_log.scroll_up();
                    Some(Action::ScrollUp)
                }
            }
```

- [ ] **Step 6: Replace `KeyCode::Down` arm (lines 1337-1351)**

Replace:

```rust
            KeyCode::Down => {
                if let Some(ref id) = self.focused_node.clone() {
                    let _trace = worker_streams.get_mut(&id.0);
                    self.detail_pane.scroll_down();
                    Some(Action::ScrollDown)
                } else if self.focused_panel == Panel::Agents {
                    if let Some(lineage) = lineage {
                        self.agents_tree.select_next(lineage);
                    }
                    Some(Action::SelectNext)
                } else {
                    self.activity_log.scroll_down(20);
                    Some(Action::ScrollDown)
                }
            }
```

with:

```rust
            KeyCode::Down => {
                if let Some(ref id) = self.focused_node.clone() {
                    let _trace = worker_streams.get_mut(&id.0);
                    self.detail_pane.scroll_down();
                    Some(Action::ScrollDown)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectNextBy(1))
                } else {
                    self.activity_log.scroll_down(20);
                    Some(Action::ScrollDown)
                }
            }
```

- [ ] **Step 7: Replace `KeyCode::PageUp` arm (lines 1353-1368)**

Replace:

```rust
            KeyCode::PageUp => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_up_by(10);
                } else if self.focused_panel == Panel::Agents {
                    if let Some(lineage) = lineage {
                        self.agents_tree.select_prev(lineage);
                        self.agents_tree.select_prev(lineage);
                        self.agents_tree.select_prev(lineage);
                        self.agents_tree.select_prev(lineage);
                        self.agents_tree.select_prev(lineage);
                    }
                } else {
                    self.activity_log.scroll_up_by(10);
                }
                Some(Action::ScrollUp)
            }
```

with:

```rust
            KeyCode::PageUp => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_up_by(10);
                    Some(Action::ScrollUp)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectPrevBy(5))
                } else {
                    self.activity_log.scroll_up_by(10);
                    Some(Action::ScrollUp)
                }
            }
```

- [ ] **Step 8: Replace `KeyCode::PageDown` arm (lines 1369-1384)**

Replace:

```rust
            KeyCode::PageDown => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_down_by(10);
                } else if self.focused_panel == Panel::Agents {
                    if let Some(lineage) = lineage {
                        self.agents_tree.select_next(lineage);
                        self.agents_tree.select_next(lineage);
                        self.agents_tree.select_next(lineage);
                        self.agents_tree.select_next(lineage);
                        self.agents_tree.select_next(lineage);
                    }
                } else {
                    self.activity_log.scroll_down_by(10, 20);
                }
                Some(Action::ScrollDown)
            }
```

with:

```rust
            KeyCode::PageDown => {
                if self.focused_node.is_some() {
                    self.detail_pane.scroll_down_by(10);
                    Some(Action::ScrollDown)
                } else if self.focused_panel == Panel::Agents {
                    let _ = lineage;
                    Some(Action::SelectNextBy(5))
                } else {
                    self.activity_log.scroll_down_by(10, 20);
                    Some(Action::ScrollDown)
                }
            }
```

### Task 1.4: Verify and commit PR1

- [ ] **Step 1: Compile**

Run: `cargo check -p spur-tui --message-format short 2>&1 | tail -20`
Expected: zero errors. Warnings about `lineage` parameter being unused inside some arms are acceptable; the `let _ = lineage;` lines should silence them.

- [ ] **Step 2: Run TUI tests**

Run: `cargo test -p spur-tui 2>&1 | tail -30`
Expected: all green. No snapshot churn (we only changed key→action plumbing, not rendering).

- [ ] **Step 3: Run integration tests**

Run: `cargo test --workspace --exclude spur-tui-e2e 2>&1 | tail -10`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/action.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/dashboard.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): consolidate lineage selection through counted Action variants

Replaces Action::SelectNext / Action::SelectPrev with
SelectNextBy(usize) / SelectPrevBy(usize). Dashboard key handlers
no longer mutate AgentsTree inline; all selection flows through
the action loop in app.rs.

This closes:
- The double-fire bug on Up/Down/j/k where the view mutated state
  inline and ALSO returned an Action that app.rs handled,
  effectively moving selection by 2 rows per keypress.
- A latent coupling on PageUp/PageDown which returned
  Action::ScrollUp/ScrollDown but relied on inline mutation; if
  anyone wires those scroll actions to do real work in app.rs,
  PageUp/PageDown would silently break.

PageUp/PageDown now emit SelectPrevBy(5) / SelectNextBy(5) — one
action per keypress, single source of truth.
EOF
)"
```

- [ ] **Step 5: Verify commit**

Run: `git log -1 --stat`
Expected: 3 files changed, only `action.rs`, `app.rs`, `dashboard.rs`.

---

## PR2 — F3 / C3: `Attempt::elapsed_at` helper

### Task 2.1: Add `Attempt::elapsed_at` with unit tests

**Files:**
- Modify: `crates/spur-core/src/lineage/types.rs` (add method on `Attempt` after line 92; add `#[cfg(test)] mod` at file end)

- [ ] **Step 1: Write failing tests**

Append to `crates/spur-core/src/lineage/types.rs`:

```rust
#[cfg(test)]
mod attempt_elapsed_tests {
    use super::*;
    use spur_acp::SessionId;
    use std::time::Duration;

    fn fixture(started: SystemTime, ended: Option<SystemTime>) -> Attempt {
        Attempt {
            session_id: SessionId("s".into()),
            started_at: started,
            ended_at: ended,
            status: AttemptStatus::Running,
            cost_usd: 0.0,
            artifacts: vec![],
            error: None,
        }
    }

    #[test]
    fn running_attempt_elapsed_uses_now() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(150);
        let a = fixture(started, None);
        assert_eq!(a.elapsed_at(now), Duration::from_secs(50));
    }

    #[test]
    fn finished_attempt_elapsed_uses_ended_at() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let ended = SystemTime::UNIX_EPOCH + Duration::from_secs(120);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(999);
        let a = fixture(started, Some(ended));
        // elapsed should freeze at ended − started, ignoring `now`.
        assert_eq!(a.elapsed_at(now), Duration::from_secs(20));
    }

    #[test]
    fn negative_skew_is_zero() {
        // If clocks skew so ended_at < started_at (rare), saturate to ZERO.
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let ended = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(300);
        let a = fixture(started, Some(ended));
        assert_eq!(a.elapsed_at(now), Duration::ZERO);
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

Run: `cargo test -p spur-core attempt_elapsed_tests 2>&1 | tail -20`
Expected: FAIL with `no method named 'elapsed_at' found for struct 'Attempt'`.

- [ ] **Step 3: Add `Attempt::elapsed_at` impl**

In `crates/spur-core/src/lineage/types.rs`, immediately after the `Attempt` struct definition (after the closing `}` of the struct on line ~92), add:

```rust
impl Attempt {
    /// Elapsed time on this attempt as of `now`. If `ended_at` is set
    /// (terminal phase observed), the result freezes at `ended_at - started_at`
    /// regardless of `now`. If `now` is somehow earlier than `started_at`
    /// (clock skew), returns `Duration::ZERO` rather than panicking.
    ///
    /// `now` is injected so callers in tests can supply a fixed clock for
    /// deterministic snapshot output.
    pub fn elapsed_at(&self, now: std::time::SystemTime) -> std::time::Duration {
        let end = self.ended_at.unwrap_or(now);
        end.duration_since(self.started_at)
            .unwrap_or(std::time::Duration::ZERO)
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core attempt_elapsed_tests 2>&1 | tail -10`
Expected: 3 passed.

### Task 2.2: Rewrite `ExecutorNode::elapsed_secs` to consult `ended_at`

**Files:**
- Modify: `crates/spur-core/src/lineage/types.rs:165-172`

- [ ] **Step 1: Add a regression test for terminal-phase freeze**

Append inside the same `#[cfg(test)] mod attempt_elapsed_tests`:

```rust
    #[test]
    fn executor_node_elapsed_secs_freezes_when_first_attempt_ended() {
        // ExecutorNode::elapsed_secs derives from the FIRST attempt's started_at.
        // When that attempt has ended_at set, elapsed must freeze.
        let started = SystemTime::now() - Duration::from_secs(60);
        let ended = SystemTime::now() - Duration::from_secs(30);
        let attempt = Attempt {
            session_id: SessionId("s".into()),
            started_at: started,
            ended_at: Some(ended),
            status: AttemptStatus::Succeeded,
            cost_usd: 0.0,
            artifacts: vec![],
            error: None,
        };
        let node = ExecutorNode {
            id: ExecutorId::new("e"),
            parent_id: None,
            child_ids: vec![],
            agent: "a".into(),
            role: spur_acp::Role::Executor,
            task_spec: String::new(),
            phase: LifecycleState::Succeeded,
            attempts: vec![attempt],
            pending_review: None,
            last_event_at: None,
            tool_call_count: 0,
            latest_tool_call: None,
            files_touched_count: 0,
            latest_diff_summary: None,
            latest_diff_text: None,
            last_error: None,
            stream_buffer: VecDeque::new(),
            issue_id: None,
            delegation_id: None,
            peer_edges: vec![],
        };
        // ended − started = 30s. Frozen.
        assert_eq!(node.elapsed_secs(), 30);
    }
```

- [ ] **Step 2: Run test to confirm failure**

Run: `cargo test -p spur-core executor_node_elapsed_secs_freezes 2>&1 | tail -15`
Expected: FAIL — `assertion 'left == right' failed: left: 60, right: 30` (the current impl uses `started_at.elapsed()` against now, returning ~60).

- [ ] **Step 3: Replace the body of `elapsed_secs`**

Replace lines 165-172 in `crates/spur-core/src/lineage/types.rs`:

```rust
    /// Seconds since this executor was spawned. Derives from the first
    /// attempt's started_at. Safe to call from render (not replay).
    pub fn elapsed_secs(&self) -> u64 {
        self.attempts
            .first()
            .and_then(|a| a.started_at.elapsed().ok().map(|d| d.as_secs()))
            .unwrap_or(0)
    }
```

with:

```rust
    /// Seconds elapsed on the executor's first attempt. Freezes at
    /// `ended_at - started_at` once the attempt is terminal; otherwise
    /// ticks against wall-clock `now`. Safe to call from render
    /// (not replay — this consults `SystemTime::now()`).
    pub fn elapsed_secs(&self) -> u64 {
        self.attempts
            .first()
            .map(|a| a.elapsed_at(SystemTime::now()).as_secs())
            .unwrap_or(0)
    }
```

- [ ] **Step 4: Run test**

Run: `cargo test -p spur-core executor_node_elapsed_secs_freezes 2>&1 | tail -10`
Expected: 1 passed.

- [ ] **Step 5: Run all spur-core tests to ensure no regression**

Run: `cargo test -p spur-core 2>&1 | tail -10`
Expected: all green.

### Task 2.3: Route `agents_tree.rs` elapsed display through `node.elapsed_secs()`

**Files:**
- Modify: `crates/spur-tui/src/components/agents_tree.rs:1-3` (imports)
- Modify: `crates/spur-tui/src/components/agents_tree.rs:261-271` (inline elapsed math)

- [ ] **Step 1: Drop the now-unused `SystemTime` import**

In `crates/spur-tui/src/components/agents_tree.rs`, replace lines 1-2:

```rust
use std::collections::HashSet;
use std::time::SystemTime;
```

with:

```rust
use std::collections::HashSet;
```

- [ ] **Step 2: Replace the inline elapsed math with `node.elapsed_secs()`**

In `crates/spur-tui/src/components/agents_tree.rs`, replace lines 261-271:

```rust
        let elapsed_str = node
            .current_attempt()
            .map(|a| {
                let now = SystemTime::now();
                let secs = now
                    .duration_since(a.started_at)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
                ;
                format!("{}m {:02}s", secs / 60, secs % 60)
            })
            .unwrap_or_default();
```

with:

```rust
        let elapsed_str = if node.attempts.is_empty() {
            String::new()
        } else {
            let secs = node.elapsed_secs();
            format!("{}m {:02}s", secs / 60, secs % 60)
        };
```

> **Note:** `node.elapsed_secs()` derives from the *first* attempt (matching `ExecutorNode::elapsed_secs` semantics), not `current_attempt`. This is a deliberate semantic alignment with the rest of the codebase (`inline_executor_card.rs`, `workers_panel.rs`). For multi-attempt executors, the elapsed shown is total wall-clock since first spawn, frozen on terminal phase. This matches user expectation ("how long has this executor been running overall").

### Task 2.4: Verify and commit PR2

- [ ] **Step 1: Compile**

Run: `cargo check --workspace --message-format short 2>&1 | tail -10`
Expected: zero errors.

- [ ] **Step 2: Run tests for affected crates**

Run: `cargo test -p spur-core -p spur-tui 2>&1 | tail -15`
Expected: all green, including the 4 new `attempt_elapsed_tests`.

- [ ] **Step 3: Manual smoke test (optional but recommended)**

Run: `cargo run --bin spur -- ` (start TUI), spawn a worker, wait for it to succeed/fail.
Expected: the elapsed time in the lineage pane stops ticking at the terminal phase. (If you can't run interactively, skip.)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/lineage/types.rs \
        crates/spur-tui/src/components/agents_tree.rs
git commit -m "$(cat <<'EOF'
fix(spur-core,spur-tui): freeze executor elapsed time at terminal phase

Adds Attempt::elapsed_at(now) which freezes at ended_at − started_at
once the attempt is terminal. ExecutorNode::elapsed_secs() now routes
through it, fixing every consumer (agents_tree, inline_executor_card,
workers_panel) at once.

Before: terminal nodes displayed "Cancelled · 1m 23s" with the seconds
still incrementing every render tick.
After: elapsed freezes at the moment the projection records ended_at.

`now` is injected on Attempt::elapsed_at so unit tests can pass a fixed
clock for deterministic output.
EOF
)"
```

---

## PR3 — F2 / B2′: Newest-first display order

### Task 3.1: Refactor `AgentsTree` to use `visible_order` as the single source of truth for traversal

**Files:**
- Modify: `crates/spur-tui/src/components/agents_tree.rs:123-176` (`visible_order`, `walk`, `render`)
- Modify: `crates/spur-tui/src/components/agents_tree.rs:178-203` (`render_subtree` — to be replaced)
- Modify: `crates/spur-tui/src/components/agents_tree.rs:342-415` (`render_lineage_to_strings`, `collect_lines`)

- [ ] **Step 1: Reverse iteration in `visible_order` and `walk`**

In `crates/spur-tui/src/components/agents_tree.rs`, replace lines 123-140:

```rust
    fn visible_order(&self, lineage: &ExecutorLineage) -> Vec<ExecutorId> {
        let mut out = Vec::new();
        for rid in lineage.root_ids() {
            self.walk(lineage, rid, &mut out);
        }
        out
    }

    fn walk(&self, l: &ExecutorLineage, id: &ExecutorId, out: &mut Vec<ExecutorId>) {
        if let Some(n) = l.node(id) {
            out.push(id.clone());
            if !self.collapsed.contains(id) {
                for c in &n.child_ids {
                    self.walk(l, c, out);
                }
            }
        }
    }
```

with:

```rust
    /// Pre-order traversal of visible nodes in display order
    /// (newest root first; within each node, newest child first).
    /// Single source of truth for tree iteration — `render` and
    /// `render_lineage_to_strings` both consume this.
    fn visible_order(&self, lineage: &ExecutorLineage) -> Vec<ExecutorId> {
        let mut out = Vec::new();
        for rid in lineage.root_ids().iter().rev() {
            self.walk(lineage, rid, &mut out);
        }
        out
    }

    fn walk(&self, l: &ExecutorLineage, id: &ExecutorId, out: &mut Vec<ExecutorId>) {
        if let Some(n) = l.node(id) {
            out.push(id.clone());
            if !self.collapsed.contains(id) {
                for c in n.child_ids.iter().rev() {
                    self.walk(l, c, out);
                }
            }
        }
    }
```

- [ ] **Step 2: Rewrite `render` to walk in display order via the canonical traversal**

The current `render` (lines 142-176) and `render_subtree` (lines 178-203) build lines via recursion and ancestor-state tracking. To consume `visible_order` while preserving the tree connector glyphs (`├─`, `└─`, `│  `), we need to compute depth + sibling-position metadata in the same reversed order. Replace lines 142-203 with:

```rust
    pub fn render(&mut self, frame: &mut Frame, area: Rect, lineage: &ExecutorLineage) {
        let block = Block::default()
            .title(" Lineage ")
            .borders(Borders::ALL)
            .border_style(focused_border_style(self.focused));

        let mut lines: Vec<Line> = Vec::new();
        // Display order: newest root first; within each subtree, newest child first.
        let roots: Vec<&ExecutorId> = lineage.root_ids().iter().rev().collect();
        for (i, rid) in roots.iter().enumerate() {
            let is_last = i == roots.len().saturating_sub(1);
            self.render_subtree(lineage, rid, 0, is_last, &[], &mut lines);
        }

        let inner_h = area.height.saturating_sub(2) as usize;
        let total = lines.len();
        let max_offset = total.saturating_sub(inner_h);

        // Keep selected item visible
        if let Some(ref sel) = self.selected {
            let order = self.visible_order(lineage);
            if let Some(idx) = order.iter().position(|id| id == sel) {
                if idx < self.scroll_offset {
                    self.scroll_offset = idx;
                } else if inner_h > 0 && idx >= self.scroll_offset + inner_h {
                    self.scroll_offset = idx.saturating_sub(inner_h - 1);
                }
            }
        }
        self.scroll_offset = self.scroll_offset.min(max_offset);

        let paragraph = Paragraph::new(lines)
            .scroll((self.scroll_offset as u16, 0))
            .block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_subtree<'a>(
        &self,
        l: &'a ExecutorLineage,
        id: &ExecutorId,
        depth: usize,
        is_last: bool,
        ancestor_states: &[bool],
        out: &mut Vec<Line<'a>>,
    ) {
        let node = match l.node(id) {
            Some(n) => n,
            None => return,
        };
        let is_selected = self.selected.as_ref() == Some(id);
        out.push(self.build_line(node, depth, is_last, ancestor_states, is_selected));
        if self.collapsed.contains(id) {
            return;
        }
        // Children walked in REVERSE so the newest child renders first.
        let children: Vec<&ExecutorId> = node.child_ids.iter().rev().collect();
        let child_count = children.len();
        for (i, c) in children.iter().enumerate() {
            let child_is_last = i == child_count.saturating_sub(1);
            let mut next_ancestors = ancestor_states.to_vec();
            next_ancestors.push(is_last);
            self.render_subtree(l, c, depth + 1, child_is_last, &next_ancestors, out);
        }
    }
```

- [ ] **Step 3: Update `render_lineage_to_strings` and `collect_lines` to also reverse**

Replace lines 342-415 in `crates/spur-tui/src/components/agents_tree.rs`:

```rust
/// Testing helper: render the lineage to plain strings.
pub fn render_lineage_to_strings(
    lineage: &ExecutorLineage,
    selected: Option<ExecutorId>,
) -> Vec<String> {
    let mut tree = AgentsTree::new();
    tree.set_selected(selected);
    let mut out = Vec::new();
    let roots = lineage.root_ids();
    for (i, rid) in roots.iter().enumerate() {
        let is_last = i == roots.len().saturating_sub(1);
        collect_lines(&tree, lineage, rid, 0, is_last, &[], &mut out);
    }
    out
}

fn collect_lines(
    tree: &AgentsTree,
    l: &ExecutorLineage,
    id: &ExecutorId,
    depth: usize,
    is_last: bool,
    ancestor_states: &[bool],
    out: &mut Vec<String>,
) {
    if let Some(node) = l.node(id) {
        let mut indent = String::new();
        for &ancestor_was_last in ancestor_states {
            if ancestor_was_last {
                indent.push_str("   ");
            } else {
                indent.push_str("│  ");
            }
        }
        let connector = if depth == 0 {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };
        let has_children = !node.child_ids.is_empty();
        let collapse_glyph = if has_children {
            if tree.collapsed.contains(&node.id) {
                "▶ "
            } else {
                "▼ "
            }
        } else {
            "  "
        };
        out.push(format!(
            "{}{}{}{} {} [{:?}]",
            indent,
            connector,
            collapse_glyph,
            node.agent,
            match node.role {
                Role::Brain => "BRAIN",
                Role::Executor => "EXEC",
                Role::SubExecutor => "SUB",
            },
            node.phase
        ));
        if !tree.collapsed.contains(id) {
            let child_count = node.child_ids.len();
            for (i, c) in node.child_ids.iter().enumerate() {
                let child_is_last = i == child_count.saturating_sub(1);
                let mut next_ancestors = ancestor_states.to_vec();
                next_ancestors.push(is_last);
                collect_lines(tree, l, c, depth + 1, child_is_last, &next_ancestors, out);
            }
        }
    }
}
```

with:

```rust
/// Testing helper: render the lineage to plain strings, in display order
/// (newest root first; within each node, newest child first).
pub fn render_lineage_to_strings(
    lineage: &ExecutorLineage,
    selected: Option<ExecutorId>,
) -> Vec<String> {
    let mut tree = AgentsTree::new();
    tree.set_selected(selected);
    let mut out = Vec::new();
    let roots: Vec<&ExecutorId> = lineage.root_ids().iter().rev().collect();
    for (i, rid) in roots.iter().enumerate() {
        let is_last = i == roots.len().saturating_sub(1);
        collect_lines(&tree, lineage, rid, 0, is_last, &[], &mut out);
    }
    out
}

fn collect_lines(
    tree: &AgentsTree,
    l: &ExecutorLineage,
    id: &ExecutorId,
    depth: usize,
    is_last: bool,
    ancestor_states: &[bool],
    out: &mut Vec<String>,
) {
    if let Some(node) = l.node(id) {
        let mut indent = String::new();
        for &ancestor_was_last in ancestor_states {
            if ancestor_was_last {
                indent.push_str("   ");
            } else {
                indent.push_str("│  ");
            }
        }
        let connector = if depth == 0 {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };
        let has_children = !node.child_ids.is_empty();
        let collapse_glyph = if has_children {
            if tree.collapsed.contains(&node.id) {
                "▶ "
            } else {
                "▼ "
            }
        } else {
            "  "
        };
        out.push(format!(
            "{}{}{}{} {} [{:?}]",
            indent,
            connector,
            collapse_glyph,
            node.agent,
            match node.role {
                Role::Brain => "BRAIN",
                Role::Executor => "EXEC",
                Role::SubExecutor => "SUB",
            },
            node.phase
        ));
        if !tree.collapsed.contains(id) {
            // Children walked in REVERSE so the newest renders first.
            let children: Vec<&ExecutorId> = node.child_ids.iter().rev().collect();
            let child_count = children.len();
            for (i, c) in children.iter().enumerate() {
                let child_is_last = i == child_count.saturating_sub(1);
                let mut next_ancestors = ancestor_states.to_vec();
                next_ancestors.push(is_last);
                collect_lines(tree, l, c, depth + 1, child_is_last, &next_ancestors, out);
            }
        }
    }
}
```

- [ ] **Step 4: Run the existing snapshot test to verify it still passes**

Run: `cargo test -p spur-tui agents_tree_snapshot 2>&1 | tail -10`
Expected: pass. (The test only asserts presence and depth-indentation, not sibling order.)

### Task 3.2: Reverse `pending_reviews` iteration in `JumpToReview` / `JumpToPreviousReview`

**Files:**
- Modify: `crates/spur-tui/src/app.rs:2011-2050`

- [ ] **Step 1: Replace `Action::JumpToReview` handler**

In `crates/spur-tui/src/app.rs`, replace lines 2011-2031:

```rust
            Action::JumpToReview => {
                // Cycle forward through pending reviews in insertion order.
                // Skip the currently-focused node so repeated presses advance
                // to the next review instead of re-landing on the same one.
                let current = self.dashboard.focused_node().cloned();
                let reviews = self.lineage.pending_reviews();
                let next = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| reviews.get(i + 1).cloned())
                    .or_else(|| reviews.into_iter().next());
                if let Some(id) = next {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
            }
```

with:

```rust
            Action::JumpToReview => {
                // Cycle forward through pending reviews in DISPLAY order
                // (newest first), so `r`/`N` flows top-to-bottom on screen
                // matching the AgentsTree visual ordering.
                let current = self.dashboard.focused_node().cloned();
                let mut reviews = self.lineage.pending_reviews();
                reviews.reverse();
                let next = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| reviews.get(i + 1).cloned())
                    .or_else(|| reviews.into_iter().next());
                if let Some(id) = next {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
            }
```

- [ ] **Step 2: Replace `Action::JumpToPreviousReview` handler**

In `crates/spur-tui/src/app.rs`, replace lines 2032-2050:

```rust
            Action::JumpToPreviousReview => {
                // Cycle backward through pending reviews in insertion order.
                let current = self.dashboard.focused_node().cloned();
                let reviews = self.lineage.pending_reviews();
                let prev = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| i.checked_sub(1).and_then(|j| reviews.get(j).cloned()))
                    .or_else(|| reviews.last().cloned());
                if let Some(id) = prev {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
            }
```

with:

```rust
            Action::JumpToPreviousReview => {
                // Cycle backward through pending reviews in DISPLAY order
                // (newest first); "previous" means visually upward on screen.
                let current = self.dashboard.focused_node().cloned();
                let mut reviews = self.lineage.pending_reviews();
                reviews.reverse();
                let prev = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| i.checked_sub(1).and_then(|j| reviews.get(j).cloned()))
                    .or_else(|| reviews.last().cloned());
                if let Some(id) = prev {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
            }
```

### Task 3.3: Re-bless any insta snapshots that capture lineage rendering

- [ ] **Step 1: Run the full test suite to surface snapshot diffs**

Run: `cargo test -p spur-tui 2>&1 | grep -E "snapshot|FAIL" | head -20`
Expected: any snapshot tests that capture lineage tree output may now diff because sibling order is reversed. The `agents_tree_snapshot.rs` test in Task 3.1 Step 4 should already pass because it only checks depth + presence.

- [ ] **Step 2: If snapshot diffs appear, review them**

Run: `cargo insta pending-snapshots -p spur-tui 2>&1 | head -30`
Expected: a list of changed `.snap.new` files. Each should show the sibling order reversed (newest first).

- [ ] **Step 3: Manually inspect each pending snapshot**

For each `.snap.new` file listed, run: `cargo insta show <file>` (or open the `.snap.new` directly).
Expected: the diff is *only* sibling-order reversal, not unrelated content changes. If you see anything other than order reversal, STOP — there's a regression.

- [ ] **Step 4: Accept snapshots only if diffs are exclusively order reversal**

If, and only if, all diffs are pure order reversal:
Run: `cargo insta accept -p spur-tui`
Expected: all `.snap.new` files are renamed to `.snap`.

### Task 3.4: Verify and commit PR3

- [ ] **Step 1: Compile**

Run: `cargo check --workspace --message-format short 2>&1 | tail -10`
Expected: zero errors.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace --exclude spur-tui-e2e 2>&1 | tail -15`
Expected: all green.

- [ ] **Step 3: Manual smoke test (optional but recommended)**

Run: `cargo run --bin spur` (start TUI), spawn three workers, wait for one to need review.
Expected:
- Newest spawn appears at the top of the lineage pane.
- Pressing `r` jumps from your current selection downward visually to the next pending review (which is older), wrapping around.
- Up/Down arrows move selection by exactly 1 row per keypress.
- PageUp/PageDown move selection by exactly 5 rows per keypress.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/agents_tree.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/snapshots/  # if any insta snapshots were re-blessed
git commit -m "$(cat <<'EOF'
fix(spur-tui): newest worker on top of lineage pane

Reverses iteration order in AgentsTree::visible_order, render, and
render_lineage_to_strings so the newest root appears at the top
and within each node the newest child renders first. The underlying
projection storage order is unchanged — replay-purity preserved.

Also reverses pending_reviews() consumption in JumpToReview /
JumpToPreviousReview so `r`/`N` cycle visually top-to-bottom on
screen, matching the new tree direction.

Snapshot tests re-blessed; only sibling-order reversal in diffs.
EOF
)"
```

---

## Risk register

| Risk | Likelihood | Mitigation |
|---|---|---|
| Worker forgets to `let _ = lineage;` and gets unused-variable warning | Medium | Plan calls it out explicitly in every Up/Down/j/k arm. |
| `Attempt::elapsed_at` test uses wrong `SystemTime::UNIX_EPOCH + offset` and overflows on 32-bit time_t | Low | Offsets are tiny (<1000s); no overflow possible. |
| Insta snapshot diffs include unrelated content (regression hidden in noise) | Medium | Task 3.3 Step 3 makes worker visually inspect each diff before accepting. |
| `node.elapsed_secs()` vs `current_attempt().started_at` semantic mismatch (first-attempt vs current-attempt elapsed) | Low | Plan calls this out explicitly with rationale; matches existing inline_executor_card / workers_panel behavior. |
| `JumpToReview` reversal breaks an existing test | Low | No tests found that exercise this code path during recon. Worker should re-run `cargo test --workspace` before committing PR3. |

## Self-review

**Spec coverage:**
- F1 → PR1 Tasks 1.1–1.4 ✓
- F2 → PR3 Tasks 3.1–3.4 ✓
- F3 → PR2 Tasks 2.1–2.4 ✓
- All findings agreed by gemini, kimi, codex are addressed.

**Type consistency:**
- `Action::SelectNextBy(usize)` used identically in action.rs definition, app.rs handler, and dashboard.rs emit sites.
- `Attempt::elapsed_at(now: SystemTime) -> Duration` signature consistent across types.rs definition, tests, and `ExecutorNode::elapsed_secs` consumer.
- `node.elapsed_secs()` returns `u64` — matches the existing `inline_executor_card::format_elapsed(secs: u64)`.

**Placeholder scan:** no TBDs, no "implement later", no skipped code — every modified arm has both before and after code shown.
