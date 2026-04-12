# TUI Mouse Scroll Hang Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the TUI hang caused by mouse scroll events starving the event loop by batching events before rendering.

**Architecture:** Replace the 1-event-per-render loop with a drain-then-render-once pattern. Add `scroll_up_by(n)` / `scroll_down_by(n)` methods so batched scroll events coalesce into a single offset adjustment.

**Tech Stack:** Rust, tokio (async runtime), crossterm (terminal events), ratatui (TUI rendering)

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/spur-tui/src/app.rs` | Event loop + mouse event dispatch |
| `crates/spur-tui/src/components/react_trace.rs` | Session trace scroll state |
| `crates/spur-tui/src/components/activity_log.rs` | Dashboard activity scroll state |
| `crates/spur-tui/src/views/session_detail.rs` | Delegates scroll to react_trace |
| `crates/spur-tui/src/views/dashboard.rs` | Delegates scroll to activity_log |

---

### Task 1: Add `scroll_up_by` / `scroll_down_by` to ReactTrace

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs:126-152`

- [ ] **Step 1: Add `scroll_up_by` method**

Add after line 129 (after existing `scroll_up`):

```rust
pub fn scroll_up_by(&mut self, lines: usize) {
    self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    self.is_following = false;
}
```

- [ ] **Step 2: Add `scroll_down_by` method**

Add after the new `scroll_up_by` (and after existing `scroll_down`):

```rust
pub fn scroll_down_by(&mut self, lines: usize) {
    let max = self.max_offset();
    self.scroll_offset = self.scroll_offset.saturating_add(lines).min(max);
    if self.scroll_offset >= max {
        self.is_following = true;
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p spur-tui`
Expected: compiles with no errors (methods are added but not yet called — dead code warning is fine)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/react_trace.rs
git commit -m "feat(tui): add scroll_up_by/scroll_down_by to ReactTrace"
```

---

### Task 2: Add `scroll_up_by` / `scroll_down_by` to ActivityLog

**Files:**
- Modify: `crates/spur-tui/src/components/activity_log.rs:46-56`

- [ ] **Step 1: Add `scroll_up_by` method**

Add after line 49 (after existing `scroll_up`):

```rust
pub fn scroll_up_by(&mut self, lines: usize) {
    self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    self.is_following = false;
}
```

- [ ] **Step 2: Add `scroll_down_by` method**

Add after line 56 (after existing `scroll_down`):

```rust
pub fn scroll_down_by(&mut self, lines: usize, visible_height: usize) {
    self.scroll_offset = self.scroll_offset.saturating_add(lines);
    if self.scroll_offset >= self.entries.len().saturating_sub(visible_height) {
        self.is_following = true;
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p spur-tui`
Expected: compiles (dead code warnings acceptable)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/activity_log.rs
git commit -m "feat(tui): add scroll_up_by/scroll_down_by to ActivityLog"
```

---

### Task 3: Expose `scroll_up_by` / `scroll_down_by` on views

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:73-79`
- Modify: `crates/spur-tui/src/views/dashboard.rs:147-153`

- [ ] **Step 1: Add methods to SessionDetailView**

Add after `scroll_down` (line 79) in `session_detail.rs`:

```rust
pub fn scroll_up_by(&mut self, lines: usize) {
    self.react_trace.scroll_up_by(lines);
}

pub fn scroll_down_by(&mut self, lines: usize) {
    self.react_trace.scroll_down_by(lines);
}
```

- [ ] **Step 2: Add methods to DashboardView**

Add after `scroll_activity_down` (line 153) in `dashboard.rs`:

```rust
pub fn scroll_activity_up_by(&mut self, lines: usize) {
    self.activity_log.scroll_up_by(lines);
}

pub fn scroll_activity_down_by(&mut self, lines: usize) {
    self.activity_log.scroll_down_by(lines, 20);
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p spur-tui`
Expected: compiles clean

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(tui): expose scroll_by methods on view layer"
```

---

### Task 4: Refactor `handle_mouse_event` to use delta-based scroll

**Files:**
- Modify: `crates/spur-tui/src/app.rs:111-132`

- [ ] **Step 1: Replace the for-loop with direct delta call**

Replace the entire `handle_mouse_event` method (lines 111-132) with:

```rust
/// Handle mouse scroll events. Only scroll wheel is processed —
/// clicks and drags are ignored to avoid tmux/terminal conflicts.
fn handle_mouse_event(&mut self, event: MouseEvent) {
    let lines: usize = match event.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => 3,
        _ => return,
    };
    let is_up = matches!(event.kind, MouseEventKind::ScrollUp);

    match self.current_view {
        ViewId::Dashboard => {
            if is_up {
                self.dashboard.scroll_activity_up_by(lines);
            } else {
                self.dashboard.scroll_activity_down_by(lines);
            }
        }
        ViewId::SessionDetail(_) => {
            if let Some(ref mut detail) = self.session_detail {
                if is_up {
                    detail.scroll_up_by(lines);
                } else {
                    detail.scroll_down_by(lines);
                }
            }
        }
    }
    self.dirty = true;
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p spur-tui`
Expected: compiles clean. The old `scroll_activity_up()`/`scroll_activity_down()` and `scroll_up()`/`scroll_down()` on the views remain (used by keyboard handlers).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "refactor(tui): mouse scroll uses delta-based scroll_by methods"
```

---

### Task 5: Implement event batching in `run_tui` event loop

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1-7` (imports)
- Modify: `crates/spur-tui/src/app.rs:335-381` (run_tui function)

- [ ] **Step 1: Add tokio::time import**

At the top of `app.rs`, change the import:

```rust
use std::time::Duration;
```

to:

```rust
use std::time::Duration;
use tokio::time::timeout;
```

(Note: `Duration` is already imported on line 1.)

- [ ] **Step 2: Replace the event loop body**

Replace the entire loop body (lines 345-377) with the batched version:

```rust
    loop {
        // Phase 1: Wait for at least one event (async yield point).
        tokio::select! {
            Some(Ok(crossterm_event)) = event_stream.next() => {
                app.handle_crossterm_event(crossterm_event);
            }
            result = event_rx.recv() => {
                match result {
                    Ok(spur_event) => {
                        app.handle_spur_event(spur_event);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        app.should_quit = true;
                    }
                }
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
        }

        // Phase 2: Drain all remaining crossterm events (non-blocking).
        // This collapses bursts of mouse scroll events into one render pass.
        loop {
            match timeout(Duration::ZERO, event_stream.next()).await {
                Ok(Some(Ok(ev))) => app.handle_crossterm_event(ev),
                _ => break,
            }
        }

        // Phase 3: Drain all remaining spur events (non-blocking).
        loop {
            match event_rx.try_recv() {
                Ok(spur_event) => app.handle_spur_event(spur_event),
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }

        // Phase 4: Single render pass.
        if app.dirty {
            terminal.draw(|f| app.render(f))?;
            app.dirty = false;
        }

        if app.should_quit {
            break;
        }
    }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p spur-tui`
Expected: compiles clean. The `timeout` import and `try_recv` usage should resolve without issue — `broadcast::Receiver` has `try_recv()` in tokio.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "fix(tui): batch events before rendering to prevent scroll livelock

The event loop previously processed one crossterm event per render pass.
Mouse scroll bursts (30-60 events/gesture) caused livelock: renders took
longer than the event interval, queue grew without bound, keyboard input
was starved.

Now: wait for first event, drain all pending events, render once."
```

---

### Task 6: Manual smoke test

**Files:** None (testing only)

- [ ] **Step 1: Build and run**

Run: `cargo build -p spur-cli && cargo run -p spur-cli`

- [ ] **Step 2: Trigger content overflow**

Send a message to the brain agent that generates a long response (e.g., "list 50 items"). Wait for content to overflow the screen.

- [ ] **Step 3: Test mouse scroll up**

Scroll up with mouse/trackpad rapidly. Verify:
- Content scrolls up smoothly
- Program remains responsive
- Pressing `Esc` immediately navigates back (no delay)
- Typing characters works immediately

- [ ] **Step 4: Test mouse scroll down**

Scroll down to bottom. Verify:
- "following" indicator reappears when reaching bottom
- New streaming content auto-scrolls again

- [ ] **Step 5: Test keyboard scroll still works**

Press `k` (up), `j` (down), `g` (top), `G` (bottom). Verify all work.

- [ ] **Step 6: Test during active streaming**

While the agent is actively generating output, scroll up with mouse. Verify:
- Scroll works without hang
- `is_following` stays false (content doesn't snap back)
- Pressing `G` resumes auto-follow

---

## Summary

| Task | What | Key Change |
|------|------|------------|
| 1 | ReactTrace scroll_by | `scroll_up_by(n)`, `scroll_down_by(n)` |
| 2 | ActivityLog scroll_by | Same pattern for dashboard |
| 3 | View delegation | Expose on SessionDetailView + DashboardView |
| 4 | Mouse handler refactor | Use delta instead of loop |
| 5 | Event batching | Drain-all-then-render-once loop |
| 6 | Smoke test | Verify fix end-to-end |
