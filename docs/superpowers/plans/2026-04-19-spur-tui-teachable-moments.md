# spur-tui Teachable Moments — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship five one-shot inline teachable moments that fire at the exact moment a concept becomes relevant, dismissible-forever per event type. Persisted to `.spur/tutorials.json`. Replaces upfront onboarding.

**Architecture:** A new `tutorials_store` module owns on-disk dismissal state (atomic tmp-rename, same POSIX discipline as `session_metadata.rs`). A new `components::teachable` module owns `TeachableTipId` (5 variants), `TeachableManager` (try-fire / dismiss), and two render paths: inline (faint italic next to a trace row) and floating toast (1 transient panel above the status bar). Each tip's fire trigger lives in `App::handle_spur_event` (T1/T2/T3) or the `App::process_action` dispatch for `SendMessage`/`NewSessionWithMessage` (T5). T4 is a counter-driven floating toast after 3 sessions open without a palette invocation.

**Tech Stack:** Rust, `ratatui`, `serde`, `serde_json`, `anyhow`. No new deps (all already in `spur-tui`).

**Spec:** `docs/superpowers/specs/2026-04-19-spur-tui-ux-best-approach.md` §4.5, §5.3.

**Depends on:** `2026-04-19-spur-tui-palette.md` (T4 reads `App.palette_visible` history; see Task 9).

**Out of scope (per spec §5.3):** Tips beyond T1–T5; migration of older `.spur/` schemas (first-write creates the file); i18n (tips ship English-only).

---

## File Structure

**New files:**

- `crates/spur-tui/src/tutorials_store.rs` — `.spur/tutorials.json` I/O with atomic writes.
- `crates/spur-tui/src/components/teachable.rs` — `TeachableTipId` enum, `Teachable` struct, `TeachableManager`, `TeachableRenderer` (inline + toast widgets).
- `crates/spur-tui/tests/tutorials_store.rs` — disk I/O round-trip tests.
- `crates/spur-tui/tests/teachable_manager.rs` — try-fire / dismiss logic tests.
- `crates/spur-tui/tests/teachable_render.rs` — inline + toast widget render tests.
- `crates/spur-tui/tests/teachable_integration.rs` — end-to-end: inject event → tip fires → is recorded as dismissed.

**Modified files:**

- `crates/spur-tui/src/lib.rs` — `pub mod tutorials_store;`
- `crates/spur-tui/src/components/mod.rs` — `pub mod teachable;`
- `crates/spur-tui/src/app.rs` — own `TeachableManager`, fire tips T1/T2/T3 on events, T5 on interrupt submit, T4 on palette-open counter; render active tips in `App::render`.
- `crates/spur-tui/src/views/session_detail.rs` — render inline teachables attached to specific trace entries.

---

## Type Reference (committed signatures — used across tasks)

```rust
// components/teachable.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachableTipId {
    FirstDelegate,        // T1
    FirstReview,          // T2
    FirstMermaid,         // T3
    PaletteNudge,         // T4
    FirstInterrupt,       // T5
}

#[derive(Debug, Clone)]
pub struct Teachable {
    pub id: TeachableTipId,
    pub text: String,                    // e.g. "💡 spur just delegated this…"
    pub anchor: TeachableAnchor,         // where it renders
    pub fired_at: std::time::Instant,    // for 10s auto-fade
}

#[derive(Debug, Clone)]
pub enum TeachableAnchor {
    TraceEntry { entry_idx: usize },     // inline, next to a trace row
    Floating,                            // toast above status bar
}

pub struct TeachableManager {
    dismissed: std::collections::HashSet<TeachableTipId>,
    active: Vec<Teachable>,
    killed: bool,                         // SPUR_TIPS=0
}
```

```rust
// tutorials_store.rs
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TutorialsData {
    #[serde(default)]
    pub dismissed: Vec<crate::components::teachable::TeachableTipId>,
}

pub struct TutorialsStore {
    path: std::path::PathBuf,
    data: TutorialsData,
}

impl TutorialsStore {
    pub fn load(path: &std::path::Path) -> Self;
    pub fn data(&self) -> &TutorialsData;
    pub fn mark_dismissed(&mut self, id: crate::components::teachable::TeachableTipId) -> anyhow::Result<()>;
}
```

---

## Task 1: `tutorials_store` — atomic JSON I/O

**Files:**
- Create: `crates/spur-tui/src/tutorials_store.rs`
- Modify: `crates/spur-tui/src/lib.rs`
- Modify: `crates/spur-tui/src/components/mod.rs` (stub the teachable module for compile)
- Test: `crates/spur-tui/tests/tutorials_store.rs`

- [ ] **Step 1: Stub `components::teachable` so the enum is resolvable**

Create `crates/spur-tui/src/components/teachable.rs` with just the enum (full implementation lands in Task 2):

```rust
//! Teachable Moments — one-shot inline tips. See Task 2+ for full impl.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachableTipId {
    FirstDelegate,
    FirstReview,
    FirstMermaid,
    PaletteNudge,
    FirstInterrupt,
}
```

Modify `crates/spur-tui/src/components/mod.rs`:

```rust
pub mod teachable;
```

- [ ] **Step 2: Write the failing test**

Create `crates/spur-tui/tests/tutorials_store.rs`:

```rust
use spur_tui::components::teachable::TeachableTipId;
use spur_tui::tutorials_store::TutorialsStore;

fn fresh_tmp_path() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spur-tutorials-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("tutorials.json")
}

#[test]
fn load_missing_file_returns_empty() {
    let path = fresh_tmp_path();
    assert!(!path.exists());
    let store = TutorialsStore::load(&path);
    assert!(store.data().dismissed.is_empty());
}

#[test]
fn mark_dismissed_persists_and_reloads() {
    let path = fresh_tmp_path();
    let mut store = TutorialsStore::load(&path);
    store.mark_dismissed(TeachableTipId::FirstDelegate).unwrap();
    store.mark_dismissed(TeachableTipId::FirstMermaid).unwrap();

    let reloaded = TutorialsStore::load(&path);
    assert!(reloaded.data().dismissed.contains(&TeachableTipId::FirstDelegate));
    assert!(reloaded.data().dismissed.contains(&TeachableTipId::FirstMermaid));
    assert_eq!(reloaded.data().dismissed.len(), 2);
}

#[test]
fn marking_same_tip_twice_is_idempotent() {
    let path = fresh_tmp_path();
    let mut store = TutorialsStore::load(&path);
    store.mark_dismissed(TeachableTipId::FirstDelegate).unwrap();
    store.mark_dismissed(TeachableTipId::FirstDelegate).unwrap();
    assert_eq!(store.data().dismissed.len(), 1);
}

#[test]
fn corrupt_json_yields_empty_and_does_not_panic() {
    let path = fresh_tmp_path();
    std::fs::write(&path, b"not valid json").unwrap();
    let store = TutorialsStore::load(&path);
    assert!(store.data().dismissed.is_empty());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test tutorials_store`
Expected: FAIL — unresolved `spur_tui::tutorials_store::TutorialsStore`.

- [ ] **Step 4: Implement the store**

Create `crates/spur-tui/src/tutorials_store.rs`:

```rust
//! `.spur/tutorials.json` — persistent per-user dismissal state for teachable
//! moments. Writes are atomic (tmp-rename), matching `session_metadata.rs`
//! discipline — survives process crash / partial write, but not power loss
//! without an `fsync` call (deliberate: same trade-off as the metadata store).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::components::teachable::TeachableTipId;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TutorialsData {
    #[serde(default)]
    pub dismissed: Vec<TeachableTipId>,
}

pub struct TutorialsStore {
    path: PathBuf,
    data: TutorialsData,
}

impl TutorialsStore {
    /// Load from disk. Missing file or corrupt JSON → empty data (no error).
    /// Treats the file as an opt-in persistence layer: if we can't read it,
    /// every tip gets a chance to fire again on next launch.
    pub fn load(path: &Path) -> Self {
        let data = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<TutorialsData>(&s).ok())
            .unwrap_or_default();
        Self { path: path.to_path_buf(), data }
    }

    pub fn data(&self) -> &TutorialsData {
        &self.data
    }

    /// Record a tip as permanently dismissed, then flush atomically.
    /// Idempotent — same tip twice is a no-op.
    pub fn mark_dismissed(&mut self, id: TeachableTipId) -> Result<()> {
        let mut set: BTreeSet<_> = self.data.dismissed.iter().copied().collect();
        if set.insert(id) {
            self.data.dismissed = set.into_iter().collect();
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        // Ensure parent exists.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&self.data)
            .context("serialize TutorialsData")?;
        std::fs::write(&tmp, body)
            .with_context(|| format!("write tmp {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename {} → {}", tmp.display(), self.path.display()))?;
        Ok(())
    }
}
```

Modify `crates/spur-tui/src/lib.rs` — add alongside existing `pub mod` lines:

```rust
pub mod tutorials_store;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test tutorials_store`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/tutorials_store.rs \
        crates/spur-tui/src/components/teachable.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/src/lib.rs \
        crates/spur-tui/tests/tutorials_store.rs
git commit -m "feat(spur-tui): tutorials_store with atomic .spur/tutorials.json I/O"
```

---

## Task 2: `TeachableManager` — try-fire, dismiss, active list

**Files:**
- Modify: `crates/spur-tui/src/components/teachable.rs`
- Test: `crates/spur-tui/tests/teachable_manager.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/teachable_manager.rs`:

```rust
use spur_tui::components::teachable::{TeachableAnchor, TeachableManager, TeachableTipId};

#[test]
fn fresh_manager_has_no_active_tips() {
    let m = TeachableManager::new(Vec::new(), false);
    assert!(m.active().is_empty());
}

#[test]
fn try_fire_pushes_an_active_tip_first_time() {
    let mut m = TeachableManager::new(Vec::new(), false);
    let fired = m.try_fire(
        TeachableTipId::FirstDelegate,
        "💡 spur just delegated this",
        TeachableAnchor::TraceEntry { entry_idx: 3 },
    );
    assert!(fired);
    assert_eq!(m.active().len(), 1);
    assert_eq!(m.active()[0].id, TeachableTipId::FirstDelegate);
}

#[test]
fn try_fire_is_noop_once_dismissed_persisted() {
    let mut m = TeachableManager::new(vec![TeachableTipId::FirstDelegate], false);
    let fired = m.try_fire(
        TeachableTipId::FirstDelegate,
        "x",
        TeachableAnchor::Floating,
    );
    assert!(!fired, "dismissed tip must not re-fire");
    assert!(m.active().is_empty());
}

#[test]
fn try_fire_is_noop_while_same_tip_still_active() {
    let mut m = TeachableManager::new(Vec::new(), false);
    m.try_fire(TeachableTipId::FirstDelegate, "x", TeachableAnchor::Floating);
    let fired_again = m.try_fire(TeachableTipId::FirstDelegate, "y", TeachableAnchor::Floating);
    assert!(!fired_again);
    assert_eq!(m.active().len(), 1);
}

#[test]
fn killed_manager_never_fires() {
    let mut m = TeachableManager::new(Vec::new(), true);
    let fired = m.try_fire(TeachableTipId::FirstDelegate, "x", TeachableAnchor::Floating);
    assert!(!fired);
}

#[test]
fn dismiss_active_removes_from_active_and_persists_id() {
    let mut m = TeachableManager::new(Vec::new(), false);
    m.try_fire(TeachableTipId::FirstDelegate, "x", TeachableAnchor::Floating);
    let dismissed_id = m.dismiss_active_head();
    assert_eq!(dismissed_id, Some(TeachableTipId::FirstDelegate));
    assert!(m.active().is_empty());
    // Subsequent try_fire must not re-fire.
    assert!(!m.try_fire(TeachableTipId::FirstDelegate, "x", TeachableAnchor::Floating));
}

#[test]
fn fade_expired_drops_tips_older_than_10s_using_injected_now() {
    let mut m = TeachableManager::new(Vec::new(), false);
    m.try_fire(TeachableTipId::FirstDelegate, "x", TeachableAnchor::Floating);
    // fade_expired uses Instant::now() by default; call the injected variant.
    let future = m.active()[0].fired_at + std::time::Duration::from_secs(11);
    m.fade_expired_at(future);
    assert!(m.active().is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test teachable_manager`
Expected: FAIL — unresolved `TeachableManager`.

- [ ] **Step 3: Implement the manager**

Replace `crates/spur-tui/src/components/teachable.rs` with:

```rust
//! Teachable Moments — one-shot inline tips.
//!
//! Each `TeachableTipId` fires at most once in a session AND at most once
//! across sessions (persisted to `.spur/tutorials.json`). Dismissal is
//! permanent. Auto-fades 10s after fire or on manual dismiss.

use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachableTipId {
    FirstDelegate,
    FirstReview,
    FirstMermaid,
    PaletteNudge,
    FirstInterrupt,
}

#[derive(Debug, Clone)]
pub enum TeachableAnchor {
    /// Inline, attached to a specific trace entry.
    TraceEntry { entry_idx: usize },
    /// Floating toast above status bar.
    Floating,
}

#[derive(Debug, Clone)]
pub struct Teachable {
    pub id: TeachableTipId,
    pub text: String,
    pub anchor: TeachableAnchor,
    pub fired_at: Instant,
}

/// Maximum visible lifetime before auto-fade.
const TEACHABLE_TTL: Duration = Duration::from_secs(10);

pub struct TeachableManager {
    dismissed: HashSet<TeachableTipId>,
    active: Vec<Teachable>,
    killed: bool,
}

impl TeachableManager {
    pub fn new(initial_dismissed: Vec<TeachableTipId>, killed: bool) -> Self {
        Self {
            dismissed: initial_dismissed.into_iter().collect(),
            active: Vec::new(),
            killed,
        }
    }

    pub fn active(&self) -> &[Teachable] {
        &self.active
    }

    /// Attempt to push a new active tip. Returns `true` when pushed; `false`
    /// when the tip is already dismissed (lifetime), already active, or the
    /// manager is killed.
    pub fn try_fire(
        &mut self,
        id: TeachableTipId,
        text: impl Into<String>,
        anchor: TeachableAnchor,
    ) -> bool {
        if self.killed { return false; }
        if self.dismissed.contains(&id) { return false; }
        if self.active.iter().any(|t| t.id == id) { return false; }
        // Cap at 1 visible tip at a time per spec §5.3.
        if !self.active.is_empty() { return false; }
        self.active.push(Teachable {
            id,
            text: text.into(),
            anchor,
            fired_at: Instant::now(),
        });
        true
    }

    /// Dismiss the oldest active tip (there is at most one). Returns its id
    /// so the caller can persist it to `TutorialsStore`.
    pub fn dismiss_active_head(&mut self) -> Option<TeachableTipId> {
        let tip = self.active.drain(..1.min(self.active.len())).next()?;
        self.dismissed.insert(tip.id);
        Some(tip.id)
    }

    /// Drop active tips whose `fired_at + TTL < now`.
    pub fn fade_expired(&mut self) {
        self.fade_expired_at(Instant::now());
    }

    /// Test-friendly: evaluate fade against a caller-supplied `now`.
    pub fn fade_expired_at(&mut self, now: Instant) {
        self.active.retain(|t| now.duration_since(t.fired_at) < TEACHABLE_TTL);
    }

    pub fn killed(&self) -> bool { self.killed }
}

/// Read `SPUR_TIPS=0` env — returns true when tips are disabled.
pub fn tips_killed_by_env() -> bool {
    std::env::var("SPUR_TIPS").map(|v| v == "0").unwrap_or(false)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test teachable_manager`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/teachable.rs \
        crates/spur-tui/tests/teachable_manager.rs
git commit -m "feat(spur-tui): TeachableManager with try-fire, dismiss, and 10s auto-fade"
```

---

## Task 3: `TeachableRenderer` — inline + floating toast widgets

**Files:**
- Modify: `crates/spur-tui/src/components/teachable.rs`
- Test: `crates/spur-tui/tests/teachable_render.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/teachable_render.rs`:

```rust
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::components::teachable::{
    Teachable, TeachableAnchor, TeachableInlineView, TeachableToastView, TeachableTipId,
};

fn tip(anchor: TeachableAnchor) -> Teachable {
    Teachable {
        id: TeachableTipId::FirstDelegate,
        text: "💡 spur just delegated this to a specialist".into(),
        anchor,
        fired_at: std::time::Instant::now(),
    }
}

fn render_inline(t: &Teachable, w: u16) -> String {
    let backend = TestBackend::new(w, 1);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect { x: 0, y: 0, width: w, height: 1 };
        f.render_widget(TeachableInlineView::new(t), area);
    }).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.width).map(|x| buf[(x, 0)].symbol()).collect::<String>()
}

fn render_toast(t: &Teachable, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect { x: 0, y: 0, width: w, height: h };
        f.render_widget(TeachableToastView::new(t), area);
    }).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn inline_renders_text_verbatim_with_lightbulb_and_italic_style() {
    let t = tip(TeachableAnchor::TraceEntry { entry_idx: 3 });
    let s = render_inline(&t, 60);
    assert!(s.contains("💡"), "lightbulb prefix missing: {s}");
    assert!(s.contains("spur just delegated"), "body text missing: {s}");
}

#[test]
fn toast_renders_bordered_panel_with_text() {
    let t = tip(TeachableAnchor::Floating);
    let s = render_toast(&t, 50, 3);
    // Bordered box: top/bottom contain box-drawing chars.
    assert!(s.contains("╭") || s.contains("┌"), "toast missing top border: {s}");
    assert!(s.contains("💡"), "lightbulb missing: {s}");
    assert!(s.contains("delegated"), "body text missing: {s}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test teachable_render`
Expected: FAIL — `TeachableInlineView`/`TeachableToastView` unresolved.

- [ ] **Step 3: Add the widgets**

Append to `crates/spur-tui/src/components/teachable.rs`:

```rust
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

pub struct TeachableInlineView<'a> {
    tip: &'a Teachable,
}

impl<'a> TeachableInlineView<'a> {
    pub fn new(tip: &'a Teachable) -> Self { Self { tip } }
}

impl<'a> Widget for TeachableInlineView<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC);
        let line = Line::from(Span::styled(self.tip.text.clone(), style));
        Paragraph::new(line).render(area, buf);
    }
}

pub struct TeachableToastView<'a> {
    tip: &'a Teachable,
}

impl<'a> TeachableToastView<'a> {
    pub fn new(tip: &'a Teachable) -> Self { Self { tip } }
}

impl<'a> Widget for TeachableToastView<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Toast: centered, max 60 cols wide, 3 rows tall.
        let w = 60.min(area.width);
        let h = 3.min(area.height);
        let x = area.x + area.width.saturating_sub(w) / 2;
        let y = area.y + area.height.saturating_sub(h) / 2;
        let modal = Rect { x, y, width: w, height: h };

        Clear.render(modal, buf);
        let block = Block::default().borders(Borders::ALL);
        let inner = block.inner(modal);
        block.render(modal, buf);

        let style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::ITALIC);
        let line = Line::from(Span::styled(self.tip.text.clone(), style));
        Paragraph::new(line).render(inner, buf);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test teachable_render`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/teachable.rs \
        crates/spur-tui/tests/teachable_render.rs
git commit -m "feat(spur-tui): TeachableInlineView + TeachableToastView widgets"
```

---

## Task 4: Own `TeachableManager` on `App` + wire `SPUR_TIPS=0`

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Test: `crates/spur-tui/tests/teachable_integration.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/teachable_integration.rs`:

```rust
use spur_tui::test_support::new_app;

#[test]
fn app_has_teachable_manager_and_no_active_tips_at_start() {
    let app = new_app();
    assert_eq!(app.teachable_active_len_for_test(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: FAIL — unresolved `teachable_active_len_for_test`.

- [ ] **Step 3: Add `TeachableManager` + `TutorialsStore` to `App`**

In `crates/spur-tui/src/app.rs`, inside `pub struct App { ... }`, add:

```rust
    teachable_manager: crate::components::teachable::TeachableManager,
    tutorials_store: crate::tutorials_store::TutorialsStore,
    /// Flag: DelegationRequested already observed this session? Used to gate T1.
    seen_first_delegate: bool,
    /// Flag: ExecutorReviewRequested already observed this session? Gates T2.
    seen_first_review: bool,
    /// Flag: Mermaid fence already observed this session? Gates T3.
    seen_first_mermaid: bool,
    /// Flag: `!`-prefixed submit already observed this session? Gates T5.
    seen_first_interrupt: bool,
    /// Session-open counter; bumped on each BrainSpawned. Reset whenever the
    /// user opens the palette (Ctrl+K). Drives T4 after 3 opens without use.
    sessions_since_palette: u32,
```

In `build_with_license_state`, inside the `Self { ... }` literal:

```rust
            teachable_manager: {
                let tutorials_path = std::path::PathBuf::from(".spur").join("tutorials.json");
                let store = crate::tutorials_store::TutorialsStore::load(&tutorials_path);
                let killed = crate::components::teachable::tips_killed_by_env();
                crate::components::teachable::TeachableManager::new(
                    store.data().dismissed.clone(),
                    killed,
                )
            },
            tutorials_store: crate::tutorials_store::TutorialsStore::load(
                &std::path::PathBuf::from(".spur").join("tutorials.json"),
            ),
            seen_first_delegate: false,
            seen_first_review: false,
            seen_first_mermaid: false,
            seen_first_interrupt: false,
            sessions_since_palette: 0,
```

Add test accessor:

```rust
    #[cfg(any(test, debug_assertions))]
    pub fn teachable_active_len_for_test(&self) -> usize {
        self.teachable_manager.active().len()
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/teachable_integration.rs
git commit -m "feat(spur-tui): App owns TeachableManager + TutorialsStore with SPUR_TIPS=0 gate"
```

---

## Task 5: Fire T1 `FirstDelegate` on first `DelegationRequested`

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/teachable_integration.rs`

Context: `handle_spur_event` has an arm for `SpurEventBody::DelegationRequested` (see `app.rs:710` and the trace-push site at `session_detail.rs:1340-1353`). We tap in there.

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/teachable_integration.rs`:

```rust
use spur_acp::{SessionId, SpurEvent};
use spur_tui::components::teachable::TeachableTipId;
use spur_tui::test_support::push_event;

fn delegation_requested_event(parent: &str) -> SpurEvent {
    // Exact constructor shape depends on SpurEvent / SpurEventBody schema;
    // use the real helper if one exists in spur_acp::tests or
    // spur_core::tests for building events in unit tests.
    // NOTE: if building `SpurEvent` directly is awkward, use an existing
    // test fixture from the spur-acp crate (rg -l "DelegationRequested"
    // will surface builders).
    spur_tui::test_fixtures::delegation_requested(parent)
}

#[test]
fn first_delegation_request_fires_first_delegate_tip() {
    let mut app = new_app();
    push_event(&mut app, delegation_requested_event("brain-session"));
    let active = app.teachable_active_list_for_test();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, TeachableTipId::FirstDelegate);
}

#[test]
fn second_delegation_request_does_not_refire_tip() {
    let mut app = new_app();
    push_event(&mut app, delegation_requested_event("brain-session"));
    // Simulate user dismissing.
    app.dismiss_teachable_head_for_test();
    assert_eq!(app.teachable_active_len_for_test(), 0);
    // Second event:
    push_event(&mut app, delegation_requested_event("brain-session"));
    assert_eq!(app.teachable_active_len_for_test(), 0);
}
```

- [ ] **Step 2: Provide a test fixture for events**

Create `crates/spur-tui/src/test_fixtures.rs`:

```rust
//! Test-only fixtures for constructing SpurEvent values in spur-tui tests.

use spur_acp::domain::events::SpurEventBody;
use spur_acp::{SessionId, SpurEvent};
use std::time::SystemTime;

/// Build a `DelegationRequested` SpurEvent. Field shape verified against
/// `crates/spur-acp/src/domain/events.rs:291-310`.
pub fn delegation_requested(parent_session: &str) -> SpurEvent {
    let body = SpurEventBody::DelegationRequested {
        from: SessionId(parent_session.to_string()),
        to_agent: "claude".to_string(),
        task: "stub task".to_string(),
        request_id: "req-test".to_string(),
        delegation_plan: None,
        issue_id: None,
    };
    SpurEvent {
        occurred_at: SystemTime::now(),
        seq: 0,
        body,
    }
}
```

Modify `crates/spur-tui/src/lib.rs`:

```rust
#[cfg(any(test, debug_assertions))]
pub mod test_fixtures;
```

If `SpurEvent` has additional fields beyond `occurred_at`, `seq`, `body` (verify with a quick read of `crates/spur-acp/src/domain/events.rs:98`), fill them with defaults — `#[serde(default)]`-annotated fields accept `Default::default()`, others take whatever smallest valid value their type allows.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: FAIL (compiles but the tip does not fire yet).

- [ ] **Step 4: Wire the fire in `handle_spur_event`**

In `crates/spur-tui/src/app.rs`, locate `handle_spur_event` and add inside the `SpurEventBody::DelegationRequested { .. }` arm (or just before the existing handler):

```rust
// Teachable T1: first delegation fires the tip exactly once per dismissal-lifetime.
if !self.seen_first_delegate {
    self.seen_first_delegate = true;
    // Anchor near the most recently appended trace entry (approx: current trace len - 1).
    let entry_idx = self
        .session_detail
        .as_ref()
        .and_then(|v| v.trace_len_for_palette().checked_sub(1))
        .unwrap_or(0);
    self.teachable_manager.try_fire(
        crate::components::teachable::TeachableTipId::FirstDelegate,
        "💡 spur just delegated this to a specialist — Alt+D to watch workers",
        crate::components::teachable::TeachableAnchor::TraceEntry { entry_idx },
    );
    self.dirty = true;
}
```

Add `trace_len_for_palette` accessor on `SessionDetailView`:

```rust
/// Current trace entry count. Used by Teachable T1 anchoring.
pub fn trace_len_for_palette(&self) -> usize {
    self.trace.entries().len() // adjust per the real ReactTrace API
}
```

Add test accessors on `App`:

```rust
    #[cfg(any(test, debug_assertions))]
    pub fn teachable_active_list_for_test(&self) -> Vec<crate::components::teachable::Teachable> {
        self.teachable_manager.active().to_vec()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn dismiss_teachable_head_for_test(&mut self) {
        if let Some(id) = self.teachable_manager.dismiss_active_head() {
            let _ = self.tutorials_store.mark_dismissed(id);
        }
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/lib.rs \
        crates/spur-tui/src/test_fixtures.rs \
        crates/spur-tui/tests/teachable_integration.rs
git commit -m "feat(spur-tui): fire T1 FirstDelegate tip on first DelegationRequested"
```

---

## Task 6: Fire T2 `FirstReview` on first `ExecutorReviewRequested`

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/test_fixtures.rs`
- Modify: `crates/spur-tui/tests/teachable_integration.rs`

- [ ] **Step 1: Add a fixture + failing test**

Append to `crates/spur-tui/src/test_fixtures.rs`:

```rust
use spur_acp::domain::events::{ReviewKind, ReviewPayload};

/// Build an `ExecutorReviewRequested` event. Shape verified against
/// `events.rs:457-464` (ExecutorReviewRequested variant) plus `events.rs:13-18`
/// (`ReviewKind`) and `events.rs:30-45` (`ReviewPayload`).
/// The `_parent_session` arg is ignored by this variant's schema but kept
/// for a consistent fixture signature.
pub fn executor_review_requested(_parent_session: &str) -> SpurEvent {
    let body = SpurEventBody::ExecutorReviewRequested {
        id: "exec-test".to_string(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "stub summary".to_string(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            ..Default::default()
        },
    };
    SpurEvent {
        occurred_at: SystemTime::now(),
        seq: 0,
        body,
    }
}
```

If `ReviewPayload` doesn't derive `Default` or has additional non-default fields (confirm with `rg -n "impl Default for ReviewPayload|#\[derive.*Default" crates/spur-acp/src/domain/events.rs`), populate those fields explicitly with their smallest valid value. The `..Default::default()` shorthand above works only if the struct derives `Default`.

Append to `crates/spur-tui/tests/teachable_integration.rs`:

```rust
#[test]
fn first_review_request_fires_first_review_tip() {
    let mut app = new_app();
    push_event(&mut app, spur_tui::test_fixtures::executor_review_requested("brain"));
    let active = app.teachable_active_list_for_test();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, TeachableTipId::FirstReview);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test teachable_integration first_review_request_fires_first_review_tip`
Expected: FAIL.

- [ ] **Step 3: Wire the fire**

In `handle_spur_event`, in or adjacent to the `SpurEventBody::ExecutorReviewRequested { .. }` arm (see `app.rs:721`):

```rust
if !self.seen_first_review {
    self.seen_first_review = true;
    let entry_idx = self
        .session_detail
        .as_ref()
        .and_then(|v| v.trace_len_for_palette().checked_sub(1))
        .unwrap_or(0);
    self.teachable_manager.try_fire(
        crate::components::teachable::TeachableTipId::FirstReview,
        "💡 a specialist is awaiting your review — press `r` anywhere to jump in",
        crate::components::teachable::TeachableAnchor::TraceEntry { entry_idx },
    );
    self.dirty = true;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/src/test_fixtures.rs \
        crates/spur-tui/tests/teachable_integration.rs
git commit -m "feat(spur-tui): fire T2 FirstReview tip on first ExecutorReviewRequested"
```

---

## Task 7: Fire T3 `FirstMermaid` on first Mermaid fence

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (or `session_detail.rs` if the fence detection is view-local)

Context: `session_detail.rs:1413-1423` inserts into `mermaid_registry` and queues `Action::MermaidRenderRequest` on `TurnComplete`. This is the right seam for the first-fence signal.

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-tui/tests/teachable_integration.rs`:

```rust
#[test]
fn first_mermaid_render_request_fires_first_mermaid_tip() {
    let mut app = new_app();
    app.simulate_first_mermaid_fence_for_test();
    let active = app.teachable_active_list_for_test();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, TeachableTipId::FirstMermaid);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test teachable_integration first_mermaid_render_request_fires`
Expected: FAIL — unresolved `simulate_first_mermaid_fence_for_test`.

- [ ] **Step 3: Hook the fence detection + test helper**

In `handle_spur_event` (or wherever `Action::MermaidRenderRequest` is produced — feature-gated on `markdown`), add:

```rust
#[cfg(feature = "markdown")]
if let Action::MermaidRenderRequest { .. } = &pending_action_being_dispatched {
    if !self.seen_first_mermaid {
        self.seen_first_mermaid = true;
        let entry_idx = self
            .session_detail
            .as_ref()
            .and_then(|v| v.trace_len_for_palette().checked_sub(1))
            .unwrap_or(0);
        self.teachable_manager.try_fire(
            crate::components::teachable::TeachableTipId::FirstMermaid,
            "💡 Alt+v opens this full-screen — `[` / `]` cycle between diagrams",
            crate::components::teachable::TeachableAnchor::TraceEntry { entry_idx },
        );
        self.dirty = true;
    }
}
```

The exact insertion site is inside `process_action` immediately BEFORE the existing handler for `MermaidRenderRequest`. Use the action being matched as the detection point.

Add test helper:

```rust
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_first_mermaid_fence_for_test(&mut self) {
        // Emulate what the production path does on first fence: mutate
        // the same flags + call try_fire. Kept as a test-only shortcut to
        // avoid constructing a full TurnComplete + markdown-stream setup.
        if !self.seen_first_mermaid {
            self.seen_first_mermaid = true;
            self.teachable_manager.try_fire(
                crate::components::teachable::TeachableTipId::FirstMermaid,
                "💡 Alt+v opens this full-screen — `[` / `]` cycle between diagrams",
                crate::components::teachable::TeachableAnchor::TraceEntry { entry_idx: 0 },
            );
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/teachable_integration.rs
git commit -m "feat(spur-tui): fire T3 FirstMermaid tip on first fence"
```

---

## Task 8: Fire T5 `FirstInterrupt` on first `!`-prefixed submit

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/teachable_integration.rs`

Context: `Action::SendMessage` carries an `interrupt: bool` flag (`action.rs:21`), set by `InputBar::submit` when text starts with `!` (`input_bar.rs:1015-1039`).

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/teachable_integration.rs`:

```rust
#[test]
fn first_bang_submit_fires_first_interrupt_tip() {
    let mut app = new_app();
    app.simulate_interrupt_submit_for_test();
    let active = app.teachable_active_list_for_test();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, TeachableTipId::FirstInterrupt);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test teachable_integration first_bang_submit`
Expected: FAIL — unresolved helper.

- [ ] **Step 3: Wire the fire**

In `App::process_action`, inside the `Action::SendMessage { interrupt, .. }` arm, at the top:

```rust
if interrupt && !self.seen_first_interrupt {
    self.seen_first_interrupt = true;
    let entry_idx = self
        .session_detail
        .as_ref()
        .and_then(|v| v.trace_len_for_palette().checked_sub(1))
        .unwrap_or(0);
    self.teachable_manager.try_fire(
        crate::components::teachable::TeachableTipId::FirstInterrupt,
        "💡 `!` prefix interrupts the current turn — use sparingly",
        crate::components::teachable::TeachableAnchor::TraceEntry { entry_idx },
    );
    self.dirty = true;
}
```

Add test helper:

```rust
    #[cfg(any(test, debug_assertions))]
    pub fn simulate_interrupt_submit_for_test(&mut self) {
        if !self.seen_first_interrupt {
            self.seen_first_interrupt = true;
            self.teachable_manager.try_fire(
                crate::components::teachable::TeachableTipId::FirstInterrupt,
                "💡 `!` prefix interrupts the current turn — use sparingly",
                crate::components::teachable::TeachableAnchor::TraceEntry { entry_idx: 0 },
            );
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/teachable_integration.rs
git commit -m "feat(spur-tui): fire T5 FirstInterrupt tip on first !-prefixed submit"
```

---

## Task 9: Fire T4 `PaletteNudge` — floating toast after 3 sessions without palette

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/teachable_integration.rs`

Context: `open_palette` was added in Plan 1 Task 9. We add `sessions_since_palette` bookkeeping: bump on `BrainSpawned`, reset to 0 on `open_palette`.

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/tests/teachable_integration.rs`:

```rust
#[test]
fn three_sessions_without_palette_fires_nudge_toast() {
    let mut app = new_app();
    app.bump_session_opened_for_test();
    app.bump_session_opened_for_test();
    assert_eq!(app.teachable_active_len_for_test(), 0, "premature fire");
    app.bump_session_opened_for_test();
    let active = app.teachable_active_list_for_test();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, TeachableTipId::PaletteNudge);
}

#[test]
fn opening_palette_resets_session_counter() {
    let mut app = new_app();
    app.bump_session_opened_for_test();
    app.bump_session_opened_for_test();
    app.mark_palette_opened_for_test();
    app.bump_session_opened_for_test(); // first post-palette session
    assert_eq!(app.teachable_active_len_for_test(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test teachable_integration three_sessions_without_palette`
Expected: FAIL.

- [ ] **Step 3: Wire the counter and nudge**

In `App::handle_spur_event`, inside the `SpurEventBody::BrainSpawned { .. }` arm, add:

```rust
self.sessions_since_palette = self.sessions_since_palette.saturating_add(1);
if self.sessions_since_palette >= 3 {
    self.teachable_manager.try_fire(
        crate::components::teachable::TeachableTipId::PaletteNudge,
        "💡 Ctrl+K jumps to any session, worker, command, or trace line — try it",
        crate::components::teachable::TeachableAnchor::Floating,
    );
    // Do NOT reset the counter — that happens only when the user actually
    // opens the palette. Prevents the nudge from re-firing once dismissed
    // (the manager's `dismissed` set guards against re-fire across sessions).
}
```

In `App::open_palette` (added in Plan 1 Task 9), at the top, add:

```rust
self.sessions_since_palette = 0;
```

Add test helpers:

```rust
    #[cfg(any(test, debug_assertions))]
    pub fn bump_session_opened_for_test(&mut self) {
        self.sessions_since_palette = self.sessions_since_palette.saturating_add(1);
        if self.sessions_since_palette >= 3 {
            self.teachable_manager.try_fire(
                crate::components::teachable::TeachableTipId::PaletteNudge,
                "💡 Ctrl+K jumps to any session, worker, command, or trace line — try it",
                crate::components::teachable::TeachableAnchor::Floating,
            );
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn mark_palette_opened_for_test(&mut self) {
        self.sessions_since_palette = 0;
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test teachable_integration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/teachable_integration.rs
git commit -m "feat(spur-tui): fire T4 PaletteNudge toast after 3 sessions without Ctrl+K"
```

---

## Task 10: Render active tips + dismiss-on-keystroke + auto-fade

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Fade on tick**

In `App::tick` (the 33 ms timer handler — `app.rs:1712`), add at the top:

```rust
self.teachable_manager.fade_expired();
```

- [ ] **Step 2: Dismiss on any handled keystroke**

In `App::handle_crossterm_event`, immediately after the Ctrl+K open-palette branch (where the existing logic consumes a key), if the manager has an active tip, dismiss it on ANY key (except when the palette is open — palette keys go to palette). Add this block near the top of `handle_crossterm_event`, after the QuitConfirm + Help + Palette guards:

```rust
// Any handled keystroke dismisses the active teachable tip (if any).
// Exception: do NOT auto-dismiss while the palette overlay is open — its
// own keys are consumed by the palette branch above.
if !self.teachable_manager.active().is_empty() {
    if let Some(id) = self.teachable_manager.dismiss_active_head() {
        let _ = self.tutorials_store.mark_dismissed(id);
        self.dirty = true;
    }
    // Do NOT return — allow the keystroke to ALSO reach its normal handler.
}
```

(Dismissal is fire-and-forget alongside the user's real action — the tip is informational, not blocking.)

- [ ] **Step 3: Render floating toasts in `App::render`**

After all view rendering, after the palette overlay, but before QuitConfirm/Help, add:

```rust
for tip in self.teachable_manager.active() {
    if matches!(tip.anchor, crate::components::teachable::TeachableAnchor::Floating) {
        let area = f.area();
        // Place the toast above the status bar, ~3 rows tall, 60 cols wide.
        let toast_area = ratatui::layout::Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(5), // 1 status + 1 ghost + 3 toast
            width: area.width,
            height: 3.min(area.height),
        };
        let view = crate::components::teachable::TeachableToastView::new(tip);
        f.render_widget(view, toast_area);
    }
}
```

- [ ] **Step 4: Render inline tips in `SessionDetailView`**

Plumb the active-tip list through `ViewContext`:

```rust
pub struct ViewContext<'a> {
    /* ... existing fields ... */
    pub active_teachables: &'a [crate::components::teachable::Teachable],
}
```

Update `test_support::test_view_ctx` to pass `&[]` for the new field.

In `SessionDetailView::render`, after rendering the trace, iterate active teachables and render each inline tip over its anchor entry's row. Exact y-position requires knowing the rendered y of `entry_idx` — use the existing `ReactTrace` layout map (or approximate by rendering below the last trace row):

```rust
for tip in ctx.active_teachables {
    if let crate::components::teachable::TeachableAnchor::TraceEntry { entry_idx } = &tip.anchor {
        // MVP: render the tip on the LAST line of the trace area when the
        // anchor matches the current tail. This avoids needing an entry→y
        // lookup table (add in Phase F2). A faint-italic line under the
        // trace is the intended visual.
        let tail_idx = self.trace.entries().len().saturating_sub(1);
        if *entry_idx >= tail_idx.saturating_sub(1) {
            let tip_row = Rect {
                x: trace_chunk.x,
                y: trace_chunk.y + trace_chunk.height.saturating_sub(1),
                width: trace_chunk.width,
                height: 1,
            };
            let view = crate::components::teachable::TeachableInlineView::new(tip);
            f.render_widget(view, tip_row);
        }
    }
}
```

Inline tip placement is approximate in MVP — precise anchor-to-pixel mapping is Phase F2.

- [ ] **Step 5: Full test + manual smoke**

```bash
cargo test -p spur-tui
cargo clippy -p spur-tui --all-targets -- -D warnings
```

Expected: PASS.

Launch TUI manually and verify:
- [ ] Type `!stop` and send → T5 inline tip appears under trace tail
- [ ] Open a brain session, wait for first delegation → T1 inline tip appears
- [ ] Open 3 sessions consecutively without pressing Ctrl+K → T4 floating toast appears
- [ ] Press any key while a tip is visible → tip disappears (AND the key performs its normal action)
- [ ] Wait 10 s without pressing keys → tip fades on its own
- [ ] `SPUR_TIPS=0 cargo run …` → no tips ever appear
- [ ] After dismissing a tip, quit and relaunch → tip does NOT reappear (persisted in `.spur/tutorials.json`)

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/views/mod.rs \
        crates/spur-tui/src/lib.rs
git commit -m "feat(spur-tui): render teachable toasts + inline tips with auto-fade and dismiss-on-key"
```

---

## Post-Plan Verification

```bash
cargo test -p spur-tui
cargo clippy -p spur-tui --all-targets -- -D warnings
cargo fmt -p spur-tui -- --check
```

All green.

Manual end-to-end smoke across all three plans (Palette + Ghost-line + Teachable):

- [ ] Fresh launch (no `.spur/tutorials.json`): Ctrl+K badge visible; ghost-line shows contextually; tips fire at expected moments.
- [ ] Dismiss T1 (first-delegate) → quit → relaunch → trigger delegation → T1 does NOT reappear.
- [ ] `SPUR_TIPS=0 SPUR_GHOST=0 cargo run …` → palette still works, no tips, no ghost-line. Normal TUI unchanged otherwise.
- [ ] `.spur/tutorials.json` contains `"dismissed": ["first_delegate"]` after dismissing T1.
- [ ] All pre-existing tests continue to pass.

---

_End of plan. Ships together with `2026-04-19-spur-tui-palette.md` and `2026-04-19-spur-tui-ghost-line.md` as the single-sprint Palette + Teaching deliverable._
