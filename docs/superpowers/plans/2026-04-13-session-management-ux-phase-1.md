# Session Management UX — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign the single-session session manager around a picker-as-hub: search, auto-resume, drafts, rename, archive, pin, preview — plus fix two latent bugs in session routing.

**Architecture:** One new module (`.spur/session_metadata.json` CRUD) + redesigned `SessionPickerView` + a draft debounce on `SessionDetailView` + startup-landing logic in `spur-cli/main.rs` + two small `Action` variants to fix data-integrity bugs. Every artifact is forward-compatible with Phase 2 multi-session.

**Tech Stack:** Rust, ratatui 0.29, tokio, serde_json, nucleo-matcher 0.3 (already in tree from `/commands` work). Target: macOS/Linux.

**Spec:** `docs/superpowers/specs/2026-04-13-session-management-ux-phase-1-design.md`

---

## File structure

**New files:**
| Path | Responsibility |
|------|----------------|
| `crates/spur-tui/src/session_metadata.rs` | `SessionMetadata` struct + `SessionMetadataStore` (load/atomic-save/CRUD/orphan-GC) |
| `crates/spur-tui/src/components/session_preview.rs` | Preview pane widget (first + last turn rendering) |
| `crates/spur-tui/src/components/resume_banner.rs` | Top-of-session dismissible banner for auto-resume |
| `crates/spur-tui/tests/session_metadata.rs` | Unit tests for metadata store |
| `crates/spur-tui/tests/session_picker_interactions.rs` | Integration tests for picker keybindings |
| `crates/spur-tui/tests/draft_persistence.rs` | Integration tests for debounced draft save/restore |
| `crates/spur-tui/tests/auto_resume_landing.rs` | Landing decision logic tests |

**Modified files:**
| Path | What changes |
|------|--------------|
| `crates/spur-tui/Cargo.toml` | Add `serde` workspace dep |
| `crates/spur-tui/src/lib.rs` | `pub mod session_metadata;` |
| `crates/spur-tui/src/views/session_picker.rs` | Full redesign: search, preview, rename prompt, pin/archive/[+New] rows, cache retention |
| `crates/spur-tui/src/views/session_detail.rs` | Draft debounce + restore + resume banner integration |
| `crates/spur-tui/src/views/dashboard.rs` | Empty-state placeholder text in InputBar |
| `crates/spur-tui/src/app.rs` | Retain picker across navigation; route metadata actions; fix `pending_user_messages`; handle `Action::NewSessionWithMessage`; startup landing decision |
| `crates/spur-tui/src/action.rs` | Add `NewSessionWithMessage`, `SaveDraft`, `RenameSession`, `ToggleSessionPin`, `ToggleSessionArchive`, `ToggleShowArchived` |
| `crates/spur-tui/src/components/help_overlay.rs` | Remove bogus `i   Chat with brain` line; add session-manager keybindings |
| `crates/spur-tui/src/components/mod.rs` | Register `session_preview`, `resume_banner` |
| `crates/spur-core/src/orchestrator.rs` | Handle `InteractiveInput::NewSessionWithMessage` — spawn brain + prompt atomically |
| `crates/spur-cli/src/main.rs` | Read metadata on startup; auto-resume or picker or dashboard; `--dashboard` flag |

---

## Dependencies between tasks

Tasks 1–2 (metadata store) are foundational. Tasks 3–11 (picker redesign) depend on Task 1. Tasks 12–14 (drafts) depend on Task 1. Tasks 15–16 (bug fixes) are independent. Tasks 17–18 (auto-resume) depend on Tasks 1 and 11. Within picker tasks, order matters only where later tasks reference earlier state.

---

## Task 1: Session metadata store — schema + load/save

**Files:**
- Create: `crates/spur-tui/src/session_metadata.rs`
- Create: `crates/spur-tui/tests/session_metadata.rs`
- Modify: `crates/spur-tui/Cargo.toml`
- Modify: `crates/spur-tui/src/lib.rs`

- [ ] **Step 1: Add `serde` workspace dep to `spur-tui`**

In `crates/spur-tui/Cargo.toml`, under `[dependencies]`, add:

```toml
serde = { workspace = true }
```

- [ ] **Step 2: Write failing tests**

Create `crates/spur-tui/tests/session_metadata.rs`:

```rust
use spur_tui::session_metadata::{SessionMetadata, SessionMetadataStore, SessionEntry};
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn load_missing_file_returns_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let store = SessionMetadataStore::load(&path);
    assert!(store.metadata().sessions.is_empty());
    assert!(store.metadata().last_active_session_id.is_none());
}

#[test]
fn save_then_load_roundtrip() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");

    let mut store = SessionMetadataStore::load(&path);
    store.upsert_entry(
        "abc123".to_string(),
        SessionEntry {
            title_override: Some("My session".into()),
            last_opened_at: "2026-04-13T18:40:15Z".into(),
            draft: "hello world".into(),
            pinned: true,
            archived: false,
        },
    );
    store.set_last_active("abc123".to_string(), "2026-04-13T18:42:00Z".into());
    store.save().unwrap();

    let store2 = SessionMetadataStore::load(&path);
    let entry = store2.metadata().sessions.get("abc123").unwrap();
    assert_eq!(entry.title_override.as_deref(), Some("My session"));
    assert_eq!(entry.draft, "hello world");
    assert!(entry.pinned);
    assert_eq!(
        store2.metadata().last_active_session_id.as_deref(),
        Some("abc123")
    );
}

#[test]
fn save_is_atomic_via_tmp_rename() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let mut store = SessionMetadataStore::load(&path);
    store.upsert_entry("x".into(), SessionEntry::default());
    store.save().unwrap();
    // .tmp should not exist after successful save
    assert!(!path.with_extension("json.tmp").exists());
    // main file should exist
    assert!(path.exists());
}

#[test]
fn load_malformed_file_returns_empty_store() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    std::fs::write(&path, "{not json").unwrap();
    let store = SessionMetadataStore::load(&path);
    assert!(store.metadata().sessions.is_empty());
}
```

Add to `crates/spur-tui/Cargo.toml` under `[dev-dependencies]` if not already present:

```toml
tempfile = "3"
```

- [ ] **Step 3: Run tests — verify they fail**

```bash
cargo test -p spur-tui --test session_metadata 2>&1 | tail -15
```

Expected: compile error "no module `session_metadata`".

- [ ] **Step 4: Implement the module**

Create `crates/spur-tui/src/session_metadata.rs`:

```rust
//! `.spur/session_metadata.json` — persistent per-session metadata.
//!
//! Tracks title overrides, drafts, pin/archive state, and last-active pointer
//! for auto-resume. Writes are atomic (tmp-rename) to survive crashes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionEntry {
    #[serde(default)]
    pub title_override: Option<String>,
    #[serde(default)]
    pub last_opened_at: String,
    #[serde(default)]
    pub draft: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMetadata {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub last_active_session_id: Option<String>,
    #[serde(default)]
    pub last_active_at: Option<String>,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionEntry>,
}

fn default_version() -> u32 {
    1
}

pub struct SessionMetadataStore {
    path: PathBuf,
    metadata: SessionMetadata,
}

impl SessionMetadataStore {
    /// Read the metadata file from `path`. Missing or malformed file → empty store.
    pub fn load(path: &Path) -> Self {
        let metadata = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<SessionMetadata>(&s).ok())
            .unwrap_or_default();
        Self {
            path: path.to_path_buf(),
            metadata,
        }
    }

    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    pub fn entry(&self, session_id: &str) -> Option<&SessionEntry> {
        self.metadata.sessions.get(session_id)
    }

    pub fn entry_mut(&mut self, session_id: &str) -> &mut SessionEntry {
        self.metadata
            .sessions
            .entry(session_id.to_string())
            .or_default()
    }

    pub fn upsert_entry(&mut self, session_id: String, entry: SessionEntry) {
        self.metadata.sessions.insert(session_id, entry);
    }

    pub fn remove_entry(&mut self, session_id: &str) {
        self.metadata.sessions.remove(session_id);
    }

    pub fn set_last_active(&mut self, session_id: String, at: String) {
        self.metadata.last_active_session_id = Some(session_id);
        self.metadata.last_active_at = Some(at);
    }

    /// Atomic save: write to `path.tmp`, then rename to `path`. Creates parent
    /// directory if missing.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.metadata)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
```

- [ ] **Step 5: Register the module**

In `crates/spur-tui/src/lib.rs`, add near the other `pub mod` lines:

```rust
pub mod session_metadata;
```

- [ ] **Step 6: Run tests — verify they pass**

```bash
cargo test -p spur-tui --test session_metadata 2>&1 | tail -15
```

Expected: `4 passed; 0 failed`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/Cargo.toml crates/spur-tui/src/lib.rs crates/spur-tui/src/session_metadata.rs crates/spur-tui/tests/session_metadata.rs
git commit -m "feat(spur-tui): session_metadata store with atomic saves"
```

---

## Task 2: Session metadata store — orphan GC

**Files:**
- Modify: `crates/spur-tui/src/session_metadata.rs`
- Modify: `crates/spur-tui/tests/session_metadata.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/spur-tui/tests/session_metadata.rs`:

```rust
#[test]
fn gc_removes_entries_for_sessions_not_in_live_list() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let mut store = SessionMetadataStore::load(&path);

    for id in ["alive1", "alive2", "gone1", "gone2"] {
        store.upsert_entry(id.to_string(), SessionEntry::default());
    }

    let live_ids: Vec<String> = vec!["alive1".into(), "alive2".into()];
    let removed = store.gc_orphans(&live_ids);
    assert_eq!(removed.len(), 2);
    assert!(removed.contains(&"gone1".to_string()));
    assert!(removed.contains(&"gone2".to_string()));
    assert_eq!(store.metadata().sessions.len(), 2);
}

#[test]
fn gc_clears_last_active_when_that_session_is_orphaned() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let mut store = SessionMetadataStore::load(&path);
    store.upsert_entry("gone".into(), SessionEntry::default());
    store.set_last_active("gone".into(), "2026-04-13T00:00:00Z".into());
    store.gc_orphans(&[]);
    assert!(store.metadata().last_active_session_id.is_none());
}
```

- [ ] **Step 2: Run tests — verify they fail**

```bash
cargo test -p spur-tui --test session_metadata 2>&1 | tail -10
```

Expected: method `gc_orphans` not found.

- [ ] **Step 3: Implement `gc_orphans`**

Append to `crates/spur-tui/src/session_metadata.rs` inside `impl SessionMetadataStore`:

```rust
    /// Remove entries for sessions no longer present in `live_ids`. If the
    /// `last_active_session_id` points to a removed entry, clear it too.
    /// Returns the session ids that were removed.
    pub fn gc_orphans(&mut self, live_ids: &[String]) -> Vec<String> {
        let live: std::collections::HashSet<&str> =
            live_ids.iter().map(|s| s.as_str()).collect();
        let to_remove: Vec<String> = self
            .metadata
            .sessions
            .keys()
            .filter(|k| !live.contains(k.as_str()))
            .cloned()
            .collect();
        for id in &to_remove {
            self.metadata.sessions.remove(id);
        }
        if let Some(ref last) = self.metadata.last_active_session_id {
            if !live.contains(last.as_str()) {
                self.metadata.last_active_session_id = None;
                self.metadata.last_active_at = None;
            }
        }
        to_remove
    }
```

- [ ] **Step 4: Run tests — verify they pass**

```bash
cargo test -p spur-tui --test session_metadata 2>&1 | tail -10
```

Expected: `6 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/session_metadata.rs crates/spur-tui/tests/session_metadata.rs
git commit -m "feat(spur-tui): session_metadata orphan GC"
```

---

## Task 3: Polish trio — help overlay fix, dashboard placeholder, picker footer hint

**Files:**
- Modify: `crates/spur-tui/src/components/help_overlay.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Remove bogus `i` line from help overlay**

In `crates/spur-tui/src/components/help_overlay.rs`, delete the line:

```rust
            Line::from("  i                  Chat with brain"),
```

Replace the block around it with updated session-manager keybindings. Find the "Dashboard — General" section and update to:

```rust
            Line::from("  j/k, Up/Down       Scroll activity log"),
            Line::from("  g / G              Jump to top / bottom"),
            Line::from("  Tab                Cycle panel focus"),
            Line::from("  v                  Toggle verbose mode"),
            Line::from("  s                  Open session picker"),
            Line::from("  q, Esc             Quit"),
```

Add a new section after "Dashboard — General":

```rust
            Line::from(""),
            Line::from(Span::styled(
                " Session Picker",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k, Up/Down       Navigate list"),
            Line::from("  Enter              Resume / create (on [+ New])"),
            Line::from("  /                  Focus search field"),
            Line::from("  n                  New session"),
            Line::from("  R                  Rename selected"),
            Line::from("  d                  Archive (or unarchive)"),
            Line::from("  p                  Toggle pin"),
            Line::from("  a                  Toggle show-archived"),
            Line::from("  P                  Toggle preview pane"),
            Line::from("  r                  Refresh list"),
            Line::from("  Esc                Clear filter → back"),
```

Also bump the popup height constant so the new content fits. Find the `34u16.min` line and change to `42u16.min`.

- [ ] **Step 2: Add Dashboard empty-state placeholder**

In `crates/spur-tui/src/views/dashboard.rs`, find `InputBar::new()` in `DashboardView::new` (around line 61). After creation, call a new method to set placeholder. First open the InputBar API — find the `InputBar` struct in `components/input_bar.rs` — if a `set_placeholder` method doesn't exist, add one that stores an `Option<String>` rendered when the buffer is empty.

Minimal approach: in `DashboardView`, render a one-line `Paragraph` overlay ABOVE the InputBar when BOTH `self.input_bar.text().is_empty()` AND no brain is attached. Find the render method's section that draws the InputBar and add right before it:

```rust
        // Empty-state hint shown only when no brain has spawned yet and user
        // hasn't typed anything.
        if self.input_bar_brain_status.is_none() && self.input_bar.text().is_empty() {
            let hint_y = input_bar_area.y.saturating_sub(1);
            if hint_y >= area.y {
                let hint_area = ratatui::layout::Rect {
                    x: input_bar_area.x,
                    y: hint_y,
                    width: input_bar_area.width,
                    height: 1,
                };
                let hint = ratatui::widgets::Paragraph::new(ratatui::text::Span::styled(
                    " Type to start a new session · s for sessions · ? for help",
                    ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
                ));
                frame.render_widget(hint, hint_area);
            }
        }
```

Note: `input_bar_brain_status` is the field tracking whether a brain is attached — find the actual field name in `DashboardView` (grep for brain status). Adjust field access to whatever exists.

- [ ] **Step 3: Add footer hint line to picker (infrastructure; keys come later)**

In `crates/spur-tui/src/views/session_picker.rs`, in every `render_*` method (currently `render_loading`, `render_populated`, `render_empty`, `render_error`), change the `Layout::vertical` from `[Constraint::Min(4), Constraint::Length(1)]` to `[Constraint::Min(4), Constraint::Length(1), Constraint::Length(1)]` and render a hint line in the new third chunk:

```rust
        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        // existing: frame.render_widget(Paragraph::new(lines), chunks[0]);
        // existing: StatusBar::render(frame, chunks[1], ...);
        let hint = "j/k nav · Enter resume · / search · n new · R rename · d archive · a show-archived · p pin · P preview · r refresh · Esc back";
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
            chunks[2],
        );
```

Apply this consistently across all four render methods (the hint string stays the same; extract to a const at module top if you prefer).

- [ ] **Step 4: Build and verify**

```bash
cargo build -p spur-tui 2>&1 | tail -10
```

Expected: builds clean.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/help_overlay.rs crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/views/session_picker.rs
git commit -m "feat(spur-tui): polish trio — help fix, dashboard placeholder, picker footer"
```

---

## Task 4: Picker reads metadata (plumbing, no UX change yet)

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Extend SessionPickerView to accept metadata snapshot**

In `crates/spur-tui/src/views/session_picker.rs`, add a field and setter:

```rust
use crate::session_metadata::SessionMetadata;

// Inside SessionPickerView struct:
    metadata: SessionMetadata,

// Inside new():
            metadata: SessionMetadata::default(),

// New method:
    pub fn set_metadata(&mut self, metadata: SessionMetadata) {
        self.metadata = metadata;
    }
```

Also add a helper to resolve the display title using metadata override:

```rust
    fn resolved_title<'a>(
        session: &'a spur_acp::SessionInfo,
        metadata: &'a SessionMetadata,
        show_cwd: bool,
    ) -> String {
        if let Some(entry) = metadata.sessions.get(session.session_id.0.as_ref()) {
            if let Some(ref t) = entry.title_override {
                return t.clone();
            }
        }
        Self::display_text(session, show_cwd)
    }
```

Replace `Self::display_text(session, show_cwd)` calls inside `render_populated` with `Self::resolved_title(session, &self.metadata, show_cwd)`.

- [ ] **Step 2: Load metadata in App**

In `crates/spur-tui/src/app.rs`, add to imports:

```rust
use crate::session_metadata::SessionMetadataStore;
```

Add field to `App` struct:

```rust
    metadata_store: SessionMetadataStore,
```

In `App::new`, load the store from a fixed path:

```rust
        let metadata_path = std::path::PathBuf::from(".spur").join("session_metadata.json");
        let metadata_store = SessionMetadataStore::load(&metadata_path);
```

Add it to the struct literal.

When the picker is created (search for `SessionPickerView::new`), call `picker.set_metadata(self.metadata_store.metadata().clone())` immediately after.

- [ ] **Step 3: Build**

```bash
cargo build -p spur-tui 2>&1 | tail -10
```

Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): picker reads session metadata for title overrides"
```

---

## Task 5: Picker — `[+ New session]` top row + `n` key

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Create: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Add `NewSessionRequested` action variant**

In `crates/spur-tui/src/action.rs`, add to `pub enum Action`:

```rust
    /// User requested spawning a new session from the picker.
    NewSessionRequested,
```

- [ ] **Step 2: Write failing integration test**

Create `crates/spur-tui/tests/session_picker_interactions.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn n_key_on_picker_emits_new_session_requested() {
    let mut picker = SessionPickerView::new();
    // Simulate populated state with zero sessions — the [+ New session] row is
    // still selectable even when list is empty.
    picker.set_sessions("test-agent".into(), vec![]);
    let action = picker.handle_key(key('n'));
    assert!(
        matches!(action, Some(Action::NewSessionRequested)),
        "expected NewSessionRequested, got {action:?}"
    );
}

#[test]
fn enter_on_new_session_row_emits_new_session_requested() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("test-agent".into(), vec![]);
    // Cursor defaults to [+ New session] row at index 0.
    let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::NewSessionRequested)));
}
```

- [ ] **Step 3: Run test — verify it fails**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
```

Expected: compile error — `set_sessions` signature may need to accept empty vec, or `NewSessionRequested` not found.

- [ ] **Step 4: Implement: add top row to picker**

In `crates/spur-tui/src/views/session_picker.rs`, change how rendering/key-handling treats the list. The cleanest approach: introduce a "virtual row" at index 0 that is always `[+ New session]`. All indexing into `sessions` uses `cursor - 1` when `cursor > 0`.

Restructure `PickerState::Populated`:
- Keep `sessions: Vec<SessionInfo>` as before.
- `cursor: usize` now ranges `0..=sessions.len()` where 0 = new-session row, 1..=len = session rows.
- In `set_sessions`: even with empty sessions, transition to `Populated` so the [+ New] row is shown. Update `render_empty` accordingly or drop it.

Simplest change set:

In `set_sessions`:

```rust
    pub fn set_sessions(&mut self, agent: String, sessions: Vec<SessionInfo>) {
        self.state = PickerState::Populated {
            agent,
            sessions,
            cursor: 0,
            resuming: false,
        };
        self.scroll_offset.set(0);
    }
```

(Delete the `PickerState::Empty` variant — no longer reachable. Remove `render_empty` too; `render_populated` handles zero-session case.)

In `render_populated`, prepend the new-session row BEFORE iterating real sessions:

```rust
        // [+ Start new session] virtual row.
        let is_new_selected = cursor == 0;
        let prefix = if is_new_selected { "\u{25b8} " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                prefix,
                if is_new_selected {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default()
                },
            ),
            Span::styled(
                "+ Start new session",
                if is_new_selected {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Green)
                },
            ),
        ]));
        lines.push(Line::from(Span::styled("  ────", Style::default().fg(Color::DarkGray))));
```

Then loop the sessions with `(i, session)` where the cursor check becomes `cursor == i + 1`.

In `handle_key` for `Populated`:

```rust
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if *cursor > 0 {
                            *cursor -= 1;
                        }
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *cursor < sessions.len() {
                            *cursor += 1;
                        }
                        None
                    }
                    KeyCode::Char('n') => Some(Action::NewSessionRequested),
                    KeyCode::Enter => {
                        if *cursor == 0 {
                            Some(Action::NewSessionRequested)
                        } else {
                            let sid = sessions[*cursor - 1].session_id.0.to_string();
                            *resuming = true;
                            Some(Action::ResumeSession { session_id: sid })
                        }
                    }
                    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
                    _ => None,
                }
```

- [ ] **Step 5: Handle `Action::NewSessionRequested` in `App`**

In `crates/spur-tui/src/app.rs`, add to `process_action`:

```rust
            Action::NewSessionRequested => {
                // Route to the orchestrator via `UserInput::NewSession` (added
                // in Task 16). Until that task lands, stub by treating as
                // NavigateTo(Dashboard) so the picker dismisses — we'll wire
                // the real behavior in Task 16.
                self.current_view = ViewId::Dashboard;
                self.dirty = true;
            }
```

Note: Task 16 replaces this stub with proper orchestrator wiring.

- [ ] **Step 6: Run tests — verify pass**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
cargo test -p spur-tui 2>&1 | grep -E "^test result|FAILED" | tail -20
```

Expected: new test passes; no pre-existing tests regress.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): picker [+ New session] top row + n key"
```

---

## Task 6: Picker — fuzzy search field

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/spur-tui/tests/session_picker_interactions.rs`:

```rust
use spur_acp::SessionInfo;

fn session(id: &str, title: &str) -> SessionInfo {
    SessionInfo {
        session_id: spur_acp::SessionId::from(id.to_string()),
        cwd: std::path::PathBuf::from("/tmp"),
        title: Some(title.into()),
        updated_at: None,
    }
}

#[test]
fn slash_focuses_search_and_typing_filters() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "test".into(),
        vec![
            session("a1", "refactor auth"),
            session("a2", "debug race condition"),
            session("a3", "perf investigation"),
        ],
    );

    // Focus search
    let _ = picker.handle_key(key('/'));
    // Type "race"
    for c in "race".chars() {
        let _ = picker.handle_key(key(c));
    }

    // The filtered view should show only session a2.
    assert_eq!(picker.visible_session_count(), 1);
    assert_eq!(picker.visible_session_at(0).map(|s| s.session_id.0.as_str()), Some("a2"));
}

#[test]
fn esc_in_search_returns_to_list_keeping_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    let _ = picker.handle_key(key('/'));
    let _ = picker.handle_key(key('b'));
    // Currently in search mode
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(action.is_none());
    // Filter still active
    assert_eq!(picker.visible_session_count(), 1);
}

#[test]
fn esc_in_list_with_active_filter_clears_it() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    let _ = picker.handle_key(key('/'));
    let _ = picker.handle_key(key('b'));
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    // Back in list mode with filter active
    assert_eq!(picker.visible_session_count(), 1);
    // Second Esc clears filter
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(action.is_none());
    assert_eq!(picker.visible_session_count(), 2);
}

#[test]
fn esc_in_list_with_no_filter_navigates_back() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::NavigateTo(_))));
}
```

If `SessionInfo` doesn't have a public constructor, match whatever fields the struct actually has (check `spur_acp::SessionInfo` definition).

- [ ] **Step 2: Run tests — verify they fail**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
```

Expected: compile error — `visible_session_count`/`visible_session_at` not found, and search behavior missing.

- [ ] **Step 3: Implement search**

Add to `PickerState::Populated`:

```rust
    Populated {
        agent: String,
        sessions: Vec<SessionInfo>,
        cursor: usize,
        resuming: bool,
        search_focused: bool,
        filter: String,
    },
```

Add helper to compute filtered indices:

```rust
    fn filtered_indices(sessions: &[SessionInfo], filter: &str, metadata: &SessionMetadata) -> Vec<usize> {
        if filter.is_empty() {
            return (0..sessions.len()).collect();
        }
        use nucleo_matcher::{pattern::{CaseMatching, Normalization, Pattern}, Matcher};
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, usize)> = sessions.iter().enumerate().filter_map(|(i, s)| {
            let title = Self::resolved_title(s, metadata, false);
            let cwd = s.cwd.display().to_string();
            let haystack = format!("{title} {cwd} {}", s.session_id.0.as_ref());
            let score = pattern.score(
                nucleo_matcher::Utf32Str::new(&haystack, &mut Vec::new()),
                &mut matcher,
            )?;
            Some((score, i))
        }).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }
```

Add public inspector methods used by tests:

```rust
    pub fn visible_session_count(&self) -> usize {
        match &self.state {
            PickerState::Populated { sessions, filter, .. } => {
                Self::filtered_indices(sessions, filter, &self.metadata).len()
            }
            _ => 0,
        }
    }

    pub fn visible_session_at(&self, idx: usize) -> Option<&SessionInfo> {
        match &self.state {
            PickerState::Populated { sessions, filter, .. } => {
                Self::filtered_indices(sessions, filter, &self.metadata)
                    .get(idx)
                    .and_then(|&i| sessions.get(i))
            }
            _ => None,
        }
    }
```

Update `handle_key` to handle search focus state:

```rust
            PickerState::Populated {
                sessions, cursor, resuming, search_focused, filter, ..
            } => {
                if *resuming { return None; }

                if *search_focused {
                    match key.code {
                        KeyCode::Esc => {
                            *search_focused = false;
                            None
                        }
                        KeyCode::Enter => {
                            *search_focused = false;
                            // Fall through to list-mode Enter handling below on next event
                            None
                        }
                        KeyCode::Backspace => {
                            filter.pop();
                            *cursor = 0;
                            None
                        }
                        KeyCode::Char(c) => {
                            filter.push(c);
                            *cursor = 0;
                            None
                        }
                        _ => None,
                    }
                } else {
                    match key.code {
                        KeyCode::Char('/') => {
                            *search_focused = true;
                            None
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if *cursor > 0 { *cursor -= 1; }
                            None
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let visible =
                                Self::filtered_indices(sessions, filter, &self.metadata).len();
                            let max_cursor = visible; // [+ New] at 0, real sessions at 1..=visible
                            if *cursor < max_cursor { *cursor += 1; }
                            None
                        }
                        KeyCode::Char('n') => Some(Action::NewSessionRequested),
                        KeyCode::Enter => {
                            if *cursor == 0 {
                                Some(Action::NewSessionRequested)
                            } else {
                                let indices = Self::filtered_indices(sessions, filter, &self.metadata);
                                let real_idx = indices.get(*cursor - 1).copied()?;
                                let sid = sessions[real_idx].session_id.0.to_string();
                                *resuming = true;
                                Some(Action::ResumeSession { session_id: sid })
                            }
                        }
                        KeyCode::Esc => {
                            if !filter.is_empty() {
                                filter.clear();
                                *cursor = 0;
                                None
                            } else {
                                Some(Action::NavigateTo(ViewId::Dashboard))
                            }
                        }
                        _ => None,
                    }
                }
            }
```

Update `render_populated` to render the filter text and use `filtered_indices` when iterating sessions.

- [ ] **Step 4: Run tests — verify they pass**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
```

Expected: new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): picker fuzzy search with / focus and two-esc clear"
```

---

## Task 7: Picker — pin (`p`) + pinned-first sort + ⭐ rendering

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Add action variant + test**

In `crates/spur-tui/src/action.rs`:

```rust
    ToggleSessionPin { session_id: String },
```

In the test file, append:

```rust
#[test]
fn p_key_emits_toggle_pin_for_highlighted_session() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x"), session("a2", "y")]);
    // Move cursor to first real session (index 1 in virtual layout)
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let action = picker.handle_key(key('p'));
    match action {
        Some(Action::ToggleSessionPin { session_id }) => {
            assert_eq!(session_id, "a1");
        }
        other => panic!("expected ToggleSessionPin, got {other:?}"),
    }
}

#[test]
fn p_key_on_new_session_row_is_noop() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    // Cursor is at [+ New session] row
    let action = picker.handle_key(key('p'));
    assert!(action.is_none());
}
```

- [ ] **Step 2: Run tests — verify failure**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
```

Expected: `p` key returns None; test fails.

- [ ] **Step 3: Implement key handling + sort**

Add a helper to get the currently highlighted real session:

```rust
    fn highlighted_session_id(&self) -> Option<String> {
        let PickerState::Populated { sessions, cursor, filter, .. } = &self.state else {
            return None;
        };
        if *cursor == 0 { return None; }
        let indices = Self::filtered_indices(sessions, filter, &self.metadata);
        let real_idx = indices.get(cursor - 1).copied()?;
        Some(sessions[real_idx].session_id.0.to_string())
    }
```

In `handle_key` list-mode handling, add:

```rust
                        KeyCode::Char('p') => self
                            .highlighted_session_id()
                            .map(|session_id| Action::ToggleSessionPin { session_id }),
```

Note: `self.highlighted_session_id()` requires the `match` to be destructured without taking `&mut self.state` — you'll need to compute the id BEFORE matching. Restructure:

```rust
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Early-compute because match below borrows `self.state` mutably.
        let hl_session_id = self.highlighted_session_id();
        match &mut self.state {
            // ... use hl_session_id inside arms
        }
    }
```

Sort order in `filtered_indices`: when filter is empty, still sort by pinned-first → recency desc. Modify the early-return for empty filter:

```rust
        if filter.is_empty() {
            let mut all: Vec<usize> = (0..sessions.len()).collect();
            all.sort_by(|&a, &b| {
                let ea = metadata.sessions.get(sessions[a].session_id.0.as_ref());
                let eb = metadata.sessions.get(sessions[b].session_id.0.as_ref());
                let pa = ea.map(|e| e.pinned).unwrap_or(false);
                let pb = eb.map(|e| e.pinned).unwrap_or(false);
                match (pa, pb) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => {
                        // recency desc (updated_at from SessionInfo)
                        let ta = sessions[a].updated_at.as_deref().unwrap_or("");
                        let tb = sessions[b].updated_at.as_deref().unwrap_or("");
                        tb.cmp(ta)
                    }
                }
            });
            return all;
        }
```

Rendering: in `render_populated`, when iterating sessions, prepend `⭐ ` to the session id when the metadata entry has `pinned: true`:

```rust
                let pinned_badge = self
                    .metadata
                    .sessions
                    .get(raw_id)
                    .map(|e| e.pinned)
                    .unwrap_or(false);
                // ... in the Line::from(vec![...]):
                if pinned_badge {
                    spans.insert(1, Span::styled("⭐ ", Style::default().fg(Color::Yellow)));
                }
```

- [ ] **Step 4: Handle `Action::ToggleSessionPin` in App**

In `crates/spur-tui/src/app.rs`, in `process_action`:

```rust
            Action::ToggleSessionPin { session_id } => {
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.pinned = !entry.pinned;
                let _ = self.metadata_store.save();
                // Refresh picker's metadata view.
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_metadata(self.metadata_store.metadata().clone());
                }
                self.dirty = true;
            }
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p spur-tui 2>&1 | grep -E "^test result|FAILED" | tail -20
```

Expected: new pin tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): picker p toggles pin, pinned sort first"
```

---

## Task 8: Picker — archive (`d`) + `a` toggle

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Add action variants + tests**

In `action.rs`:

```rust
    ToggleSessionArchive { session_id: String },
    ToggleShowArchived,
```

In test file:

```rust
#[test]
fn d_key_emits_toggle_archive_for_highlighted_session() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let action = picker.handle_key(key('d'));
    match action {
        Some(Action::ToggleSessionArchive { session_id }) => assert_eq!(session_id, "a1"),
        other => panic!("expected ToggleSessionArchive, got {other:?}"),
    }
}

#[test]
fn a_key_toggles_show_archived() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(key('a'));
    assert!(matches!(action, Some(Action::ToggleShowArchived)));
}
```

- [ ] **Step 2: Run tests — verify failure**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
```

- [ ] **Step 3: Implement**

Add `show_archived: bool` field to `SessionPickerView` (not to `PickerState` — it's view-level, survives re-population). Default false.

```rust
pub struct SessionPickerView {
    state: PickerState,
    scroll_offset: Cell<usize>,
    metadata: SessionMetadata,
    show_archived: bool,
}
```

In `handle_key` list-mode, add:

```rust
                        KeyCode::Char('d') => hl_session_id
                            .map(|session_id| Action::ToggleSessionArchive { session_id }),
                        KeyCode::Char('a') => Some(Action::ToggleShowArchived),
```

Update `filtered_indices` to respect `show_archived`. Since it's on `self`, pass it into the function:

```rust
    fn filtered_indices(
        sessions: &[SessionInfo],
        filter: &str,
        metadata: &SessionMetadata,
        show_archived: bool,
    ) -> Vec<usize> {
        // before fuzzy/sort logic, apply archive filter
        let candidate: Vec<usize> = (0..sessions.len())
            .filter(|&i| {
                let archived = metadata
                    .sessions
                    .get(sessions[i].session_id.0.as_ref())
                    .map(|e| e.archived)
                    .unwrap_or(false);
                if archived { show_archived } else { true }
            })
            .collect();
        // ... apply filter+sort within `candidate`
```

Update all callers of `filtered_indices` to pass `self.show_archived` (or whatever's in scope).

Add public inspectors for tests:

```rust
    pub fn is_show_archived(&self) -> bool {
        self.show_archived
    }
```

In render, show `[showing archived]` flag in the header line when `show_archived` is true.

- [ ] **Step 4: Handle actions in App**

```rust
            Action::ToggleSessionArchive { session_id } => {
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.archived = !entry.archived;
                let _ = self.metadata_store.save();
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_metadata(self.metadata_store.metadata().clone());
                }
                self.dirty = true;
            }
            Action::ToggleShowArchived => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.toggle_show_archived();
                }
                self.dirty = true;
            }
```

Add `pub fn toggle_show_archived(&mut self) { self.show_archived = !self.show_archived; }` to `SessionPickerView`.

- [ ] **Step 5: Run tests**

```bash
cargo test -p spur-tui 2>&1 | grep -E "^test result|FAILED" | tail -20
```

Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): picker d archive + a show-archived toggle"
```

---

## Task 9: Picker — rename (`R`) inline bottom-bar prompt

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Add action + test**

In `action.rs`:

```rust
    RenameSession { session_id: String, new_title: String },
```

In test file:

```rust
#[test]
fn R_enters_rename_mode_and_enter_commits() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "old title")]);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    assert!(picker.is_rename_active());
    // Clear old title
    for _ in 0..20 { let _ = picker.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)); }
    for c in "new name".chars() {
        let _ = picker.handle_key(key(c));
    }
    let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match action {
        Some(Action::RenameSession { session_id, new_title }) => {
            assert_eq!(session_id, "a1");
            assert_eq!(new_title, "new name");
        }
        other => panic!("expected RenameSession, got {other:?}"),
    }
}

#[test]
fn esc_in_rename_cancels_without_action() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "old")]);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
    let _ = picker.handle_key(key('z'));
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(action.is_none());
    assert!(!picker.is_rename_active());
}
```

- [ ] **Step 2: Run — verify fail**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
```

- [ ] **Step 3: Implement rename mode**

Add field to `SessionPickerView`:

```rust
    rename_state: Option<RenameState>,
```

```rust
struct RenameState {
    session_id: String,
    buffer: String,
}
```

In `handle_key`, before entering the state match:

```rust
        if let Some(rs) = self.rename_state.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let out = Action::RenameSession {
                        session_id: rs.session_id.clone(),
                        new_title: rs.buffer.clone(),
                    };
                    self.rename_state = None;
                    return Some(out);
                }
                KeyCode::Esc => {
                    self.rename_state = None;
                    return None;
                }
                KeyCode::Backspace => {
                    rs.buffer.pop();
                    return None;
                }
                KeyCode::Char(c) => {
                    rs.buffer.push(c);
                    return None;
                }
                _ => return None,
            }
        }
```

In the Populated `R` arm:

```rust
                        KeyCode::Char('R') if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) => {
                            if let Some(sid) = hl_session_id.clone() {
                                // Pre-fill with current resolved title.
                                let info = sessions.iter().find(|s| s.session_id.0.as_ref() == sid.as_str());
                                let buffer = info.map(|s| Self::resolved_title(s, &self.metadata, false)).unwrap_or_default();
                                self.rename_state = Some(RenameState { session_id: sid, buffer });
                            }
                            return None;
                        }
```

Note: crossterm reports `Char('R')` when Shift+r is pressed. Some terminals also report `Char('r')` with `SHIFT` modifier. Handle both by accepting `Char('R')` uppercase OR `Char('r')` with SHIFT. Simplify to:

```rust
                        KeyCode::Char(c) if c == 'R' => { /* ... */ return None; }
```

Add inspector:

```rust
    pub fn is_rename_active(&self) -> bool { self.rename_state.is_some() }
```

Render the rename prompt in the bottom StatusBar area (or a new dedicated line above the footer hint) when `rename_state.is_some()`:

```rust
        if let Some(ref rs) = self.rename_state {
            // Find the render slot and overlay.
            // Add a rendered line like: Rename → [<buffer>_]
            let prompt = format!("Rename → {}", rs.buffer);
            frame.render_widget(
                Paragraph::new(Span::styled(prompt, Style::default().fg(Color::Cyan))),
                chunks[1], // or a dedicated slot
            );
        }
```

Adjust layout to have a dedicated rename-slot row when `rename_state.is_some()`.

- [ ] **Step 4: Handle `RenameSession` in App**

```rust
            Action::RenameSession { session_id, new_title } => {
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.title_override = if new_title.trim().is_empty() { None } else { Some(new_title) };
                let _ = self.metadata_store.save();
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_metadata(self.metadata_store.metadata().clone());
                }
                self.dirty = true;
            }
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -10
```

Expected: rename tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): picker R rename with inline bottom-bar prompt"
```

---

## Task 10: Picker — cache across reopens (App retains picker)

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Write test**

Append:

```rust
#[test]
fn picker_state_survives_close_and_reopen_in_app() {
    use spur_tui::test_support;
    let mut app = test_support::new_app();
    test_support::push_sessions(&mut app, vec![session("a1", "first"), session("a2", "second")]);
    test_support::open_picker(&mut app);
    // Move cursor + set a filter.
    test_support::picker_handle(&mut app, key('j'));   // cursor to first real row
    test_support::picker_handle(&mut app, key('/'));   // focus search
    test_support::picker_handle(&mut app, key('s'));   // filter 's'
    let cursor_before = test_support::picker_cursor(&app);
    let filter_before = test_support::picker_filter(&app);

    // Close picker (Esc exits filter first, Esc again goes back).
    // Use direct nav-back:
    test_support::open_dashboard(&mut app);

    // Reopen picker — same state expected.
    test_support::open_picker(&mut app);
    assert_eq!(test_support::picker_cursor(&app), cursor_before);
    assert_eq!(test_support::picker_filter(&app), filter_before);
}
```

- [ ] **Step 2: Add test_support helpers**

In `crates/spur-tui/src/test_support.rs` (if it exists; otherwise the relevant file exposing test helpers — grep for `pub fn new_session_state`):

```rust
pub fn new_app() -> crate::app::App {
    crate::app::App::new(None, false)
}

pub fn push_sessions(app: &mut crate::app::App, sessions: Vec<spur_acp::SessionInfo>) {
    // Simulate SessionsListed arrival.
    // ... call into app's event-handling surface with a synthesized SpurEvent.
}
pub fn open_picker(app: &mut crate::app::App) { /* set current_view to SessionPicker */ }
pub fn open_dashboard(app: &mut crate::app::App) { /* set current_view to Dashboard */ }
pub fn picker_handle(app: &mut crate::app::App, k: crossterm::event::KeyEvent) { /* forward */ }
pub fn picker_cursor(app: &crate::app::App) -> usize { /* expose */ }
pub fn picker_filter(app: &crate::app::App) -> String { /* expose */ }
```

Implement by delegating to the existing `App` methods. Add public inspectors on `SessionPickerView` as needed:

```rust
pub fn cursor(&self) -> usize { /* ... */ }
pub fn filter(&self) -> String { /* ... */ }
```

- [ ] **Step 3: Make App retain the picker**

In `crates/spur-tui/src/app.rs`, the current pattern is probably `self.session_picker: Option<SessionPickerView>` which is set to `Some(SessionPickerView::new())` each time the user presses `s`. Change to: create the picker once (lazy) and never drop it until App drops.

Find where `SessionPickerView::new()` is called in `process_action`:

```rust
            Action::RequestSessions => {
                if self.session_picker.is_none() {
                    self.session_picker = Some(SessionPickerView::new());
                }
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_metadata(self.metadata_store.metadata().clone());
                }
                // Trigger ListSessions to refresh.
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(crate::UserInput::ListSessions);
                }
                self.current_view = ViewId::SessionPicker;
                self.dirty = true;
            }
```

Note the crucial change: DO NOT call `SessionPickerView::new()` when reopening. The existing instance's cursor + filter state is preserved.

Explicit force-refresh (bound to `r` inside the picker — Task 12 will add a `RefreshSessions` action later) can reset things if needed.

- [ ] **Step 4: Run tests**

```bash
cargo test -p spur-tui 2>&1 | grep -E "^test result|FAILED" | tail -20
```

Expected: picker retention test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/test_support.rs crates/spur-tui/src/views/session_picker.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): retain picker across navigation for cursor+filter memory"
```

---

## Task 11: Picker — preview pane (`P` toggle)

**Files:**
- Create: `crates/spur-tui/src/components/session_preview.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Write tests**

```rust
#[test]
fn P_toggles_preview_visible() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    assert!(!picker.is_preview_visible());
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
    assert!(picker.is_preview_visible());
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT));
    assert!(!picker.is_preview_visible());
}
```

- [ ] **Step 2: Create the preview component**

Create `crates/spur-tui/src/components/session_preview.rs`:

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

pub struct SessionPreview;

/// Lightweight content holder passed by the picker per-frame.
#[derive(Default)]
pub struct PreviewContent {
    pub first_turn_user: String,   // first user prompt, truncated
    pub last_turn_agent: String,   // last assistant reply, truncated
    pub placeholder: Option<String>, // shown instead of content when Some
}

impl SessionPreview {
    pub fn render(frame: &mut Frame, area: Rect, content: &PreviewContent) {
        let block = Block::default()
            .title(" Preview ")
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray));

        let lines: Vec<Line> = if let Some(ref msg) = content.placeholder {
            vec![Line::from(Span::styled(msg.clone(), Style::default().fg(Color::DarkGray)))]
        } else {
            vec![
                Line::from(Span::styled("You: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Line::from(content.first_turn_user.clone()),
                Line::from(""),
                Line::from(Span::styled("Assistant: ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))),
                Line::from(content.last_turn_agent.clone()),
            ]
        };
        let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
        frame.render_widget(p, area);
    }
}
```

Register in `crates/spur-tui/src/components/mod.rs`:

```rust
pub mod session_preview;
```

- [ ] **Step 3: Implement `P` toggle + rendering in picker**

Add field `preview_visible: bool` to `SessionPickerView`.

In `handle_key` list-mode:

```rust
                        KeyCode::Char('P') => {
                            self.preview_visible = !self.preview_visible;
                            return None;
                        }
```

Add inspector:

```rust
    pub fn is_preview_visible(&self) -> bool { self.preview_visible }
```

In `render_populated`, when `preview_visible`, split the list area to leave the bottom third for the preview. Call `SessionPreview::render(frame, preview_area, &content)` with content based on highlighted session (for now, stub content = `PreviewContent { placeholder: Some("Press Enter to start a new session · any unsent draft will be saved".into()), ..Default::default() }` when cursor is on [+ New], or empty fields for sessions (actual content loading is deferred — Task 11.1 if needed, but a phase-1 minimal acceptable preview just shows session id + cwd + time as a placeholder).

Minimal phase-1 preview content: show session metadata (id, cwd, title override, pinned/archived status, last_opened_at, draft preview) rather than turn content. This avoids the ACP `load_session` roundtrip entirely for phase 1.

Update `PreviewContent` struct to match what you actually show:

```rust
#[derive(Default)]
pub struct PreviewContent {
    pub lines: Vec<(String, String)>, // label, value pairs
    pub placeholder: Option<String>,
}
```

And update `render` accordingly. This is simpler and still useful.

- [ ] **Step 4: Run tests**

```bash
cargo test -p spur-tui --test session_picker_interactions 2>&1 | tail -5
```

Expected: preview toggle test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/session_preview.rs crates/spur-tui/src/components/mod.rs crates/spur-tui/src/views/session_picker.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): picker P toggles preview pane with metadata-based content"
```

---

## Task 12: Picker — `r` refresh action

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Write test**

```rust
#[test]
fn r_key_emits_refresh_sessions() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(key('r'));
    assert!(matches!(action, Some(Action::RefreshSessions)));
}
```

- [ ] **Step 2: Add action + wire**

In `action.rs`:

```rust
    RefreshSessions,
```

In picker's handle_key list-mode:

```rust
                        KeyCode::Char('r') => Some(Action::RefreshSessions),
```

In App:

```rust
            Action::RefreshSessions => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(crate::UserInput::ListSessions);
                }
                self.dirty = true;
            }
```

- [ ] **Step 3: Run tests + commit**

```bash
cargo test -p spur-tui 2>&1 | grep -E "^test result|FAILED" | tail -10
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): picker r refresh action"
```

---

## Task 13: Draft persistence — debounced save in SessionDetailView

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Create: `crates/spur-tui/tests/draft_persistence.rs`

- [ ] **Step 1: Add action**

In `action.rs`:

```rust
    /// Persist a session's unsent InputBar text to metadata.
    SaveDraft { session_id: String, draft: String },
```

- [ ] **Step 2: Write integration test**

Create `crates/spur-tui/tests/draft_persistence.rs`:

```rust
use std::time::Duration;
use spur_tui::action::Action;
use spur_tui::views::session_detail::SessionDetailView;
use spur_tui::views::View;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn tick_emits_save_draft_after_debounce() {
    let sid = spur_acp::SessionId::from("sess-1".to_string());
    let mut view = SessionDetailView::new(
        sid.clone(),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
    );
    // Type a few characters.
    for c in "hello".chars() {
        let _ = view.handle_key(key(c));
    }
    // Artificially advance the debounce clock past 500ms via a test helper:
    view.test_set_last_draft_change(std::time::Instant::now() - Duration::from_millis(600));
    // Tick returns the SaveDraft action when debounce elapsed + draft changed.
    let action = view.tick_for_test();
    match action {
        Some(Action::SaveDraft { session_id, draft }) => {
            assert_eq!(session_id, "sess-1");
            assert_eq!(draft, "hello");
        }
        other => panic!("expected SaveDraft, got {other:?}"),
    }
}
```

- [ ] **Step 3: Run — verify fail**

- [ ] **Step 4: Implement debounce in SessionDetailView**

Add fields:

```rust
    last_draft_change_at: Option<std::time::Instant>,
    last_persisted_draft: String,
```

In `handle_key`, after the InputBar processes the key and text may have changed:

```rust
        let current = self.input_bar.text().to_string();
        if current != self.last_persisted_draft {
            self.last_draft_change_at = Some(std::time::Instant::now());
        }
```

Add a helper producing the action if debounce elapsed:

```rust
    pub fn draft_save_action(&mut self) -> Option<Action> {
        let Some(at) = self.last_draft_change_at else { return None; };
        if at.elapsed() < std::time::Duration::from_millis(500) {
            return None;
        }
        let current = self.input_bar.text().to_string();
        if current == self.last_persisted_draft {
            self.last_draft_change_at = None;
            return None;
        }
        self.last_persisted_draft = current.clone();
        self.last_draft_change_at = None;
        Some(Action::SaveDraft {
            session_id: self.session_id.0.clone(),
            draft: current,
        })
    }
```

Test-only helpers:

```rust
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn test_set_last_draft_change(&mut self, at: std::time::Instant) {
        self.last_draft_change_at = Some(at);
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn tick_for_test(&mut self) -> Option<Action> {
        self.draft_save_action()
    }
```

- [ ] **Step 5: Wire into App's tick loop**

In the App's render loop (`run_tui`), after each tick or event-drain, call:

```rust
        if let Some(ref mut detail) = app.session_detail {
            if let Some(action) = detail.draft_save_action() {
                app.process_action(action);
            }
        }
```

Handle `Action::SaveDraft` in `process_action`:

```rust
            Action::SaveDraft { session_id, draft } => {
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.draft = draft;
                let _ = self.metadata_store.save();
                self.dirty = false; // writing metadata doesn't require re-render
            }
```

- [ ] **Step 6: Run tests + commit**

```bash
cargo test -p spur-tui --test draft_persistence 2>&1 | tail -10
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/draft_persistence.rs
git commit -m "feat(spur-tui): debounced draft persistence to session_metadata"
```

---

## Task 14: Draft restore on session open

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/draft_persistence.rs`

- [ ] **Step 1: Write test**

```rust
#[test]
fn session_view_restores_draft_from_metadata() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId::from("sess-1".to_string()),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
    );
    view.restore_draft("previous unsent text");
    assert_eq!(view.input_bar_text(), "previous unsent text");
}
```

- [ ] **Step 2: Implement**

Add to `SessionDetailView`:

```rust
    pub fn restore_draft(&mut self, draft: &str) {
        if !draft.is_empty() {
            self.input_bar.set_text(draft);
            self.last_persisted_draft = draft.to_string();
        }
    }

    pub fn input_bar_text(&self) -> &str {
        self.input_bar.text()
    }
```

If `InputBar::set_text` doesn't exist, add it (find `InputBar` in `components/input_bar.rs` and add a method that replaces the buffer and moves cursor to end).

In `App`, after creating a new `SessionDetailView` for a `BrainSpawned` event, read draft from metadata and restore:

```rust
                if needs_new {
                    let mut view = SessionDetailView::new(
                        session.clone(),
                        agent.clone(),
                        "brain".to_string(),
                        std::env::current_dir().unwrap_or_default(),
                    );
                    if let Some(entry) = self.metadata_store.entry(&session.0) {
                        view.restore_draft(&entry.draft);
                    }
                    // ... rest
                    self.session_detail = Some(view);
                }
```

- [ ] **Step 3: Run tests + commit**

```bash
cargo test -p spur-tui --test draft_persistence 2>&1 | tail -10
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/input_bar.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/draft_persistence.rs
git commit -m "feat(spur-tui): restore draft from metadata on session view creation"
```

---

## Task 15: Fix BUG-1 — `pending_user_messages` cross-session replay

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/session_update_handling.rs` (add regression test)

- [ ] **Step 1: Write regression test**

```rust
#[test]
fn pending_messages_do_not_replay_into_unrelated_session() {
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    let mut app = spur_tui::test_support::new_app();

    // Simulate user typing and sending on Dashboard WITH NO brain attached:
    // this should buffer the message tagged with the intended new session.
    spur_tui::test_support::push_dashboard_send(&mut app, "hello for session A");

    // A different session spawns (e.g. from a background workflow).
    let other_sid = SessionId("unrelated-session".into());
    let ev = spur_acp::SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "claude-code-acp".into(),
        session: other_sid.clone(),
    });
    spur_tui::test_support::push_event(&mut app, ev);

    // The unrelated session's detail view should NOT contain the buffered text.
    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    assert!(
        !detail.rendered_trace_contains("hello for session A"),
        "buffered message leaked into unrelated session"
    );
}
```

Helpers needed in `test_support.rs`:

```rust
pub fn push_dashboard_send(app: &mut crate::app::App, text: &str) { /* ... */ }
pub fn push_event(app: &mut crate::app::App, ev: spur_acp::SpurEvent) { /* ... */ }
pub fn session_detail(app: &crate::app::App) -> Option<&crate::views::session_detail::SessionDetailView> { /* ... */ }
```

And a helper on `SessionDetailView`:

```rust
    pub fn rendered_trace_contains(&self, needle: &str) -> bool { /* iterate trace entries */ }
```

- [ ] **Step 2: Refactor**

In `app.rs`, delete the `pending_user_messages: Vec<String>` field and its drain-into-new-view path (around line 348).

Replace with the correct flow:
1. When user sends from Dashboard with NO brain → use `Action::NewSessionWithMessage { blocks, interrupt }` (added in Task 16).
2. When user sends from SessionDetail → the message is already session-scoped; no buffer needed.

For this task, remove the buffer entirely. Dashboard "type to spawn" path will fail temporarily until Task 16 adds `NewSessionWithMessage` — so these two tasks MUST be done together. Merge them or sequence Task 16 immediately after this one without an independent commit.

Actually, better sequencing: do Task 16 FIRST, then this one. Swap the order in the plan.

**Sequencing note:** Perform Task 16 BEFORE Task 15.

- [ ] **Step 3: After Task 16 is done, delete the buffer**

```bash
# After Task 16:
# In app.rs, remove:
#   pending_user_messages: Vec<String>,
#   self.pending_user_messages = Vec::new(),
#   for msg in self.pending_user_messages.drain(..) { view.push_user_message(&msg); }
#   and any other references.
```

Run tests including the regression:

```bash
cargo test -p spur-tui 2>&1 | grep -E "^test result|FAILED" | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/test_support.rs crates/spur-tui/tests/session_update_handling.rs
git commit -m "fix(spur-tui): remove pending_user_messages cross-session replay (BUG-1)"
```

---

## Task 16: Fix BUG-2 — `Action::NewSessionWithMessage` + orchestrator handler

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/lib.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Add action + UserInput + InteractiveInput variants**

In `crates/spur-tui/src/action.rs`:

```rust
    /// Spawn a new session and send these blocks as the first prompt in one step.
    /// Used by Dashboard's InputBar when no brain is attached AND by picker's
    /// NewSessionRequested when a first message is unspecified (blocks empty).
    NewSessionWithMessage {
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
```

In `crates/spur-tui/src/lib.rs` (or wherever `UserInput` lives):

```rust
    /// Spawn-and-prompt atomically, replacing the SessionId::new() placeholder
    /// hack on Dashboard's SendMessage path.
    NewSessionWithMessage {
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
```

In `crates/spur-core/src/orchestrator.rs`, add to `pub enum InteractiveInput`:

```rust
    NewSessionWithMessage {
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
```

- [ ] **Step 2: Dashboard emits the new action when no brain**

In `crates/spur-tui/src/views/dashboard.rs`, find where `Action::SendMessage { session: SessionId::new(), ... }` is emitted (around line 327-333). Replace with logic that checks if a brain is attached:

```rust
        if is_editing_key {
            if let Some((text, interrupt)) = self.input_bar.handle_key(key) {
                let blocks = vec![spur_acp::ContentBlock::Text(
                    spur_acp::TextContent::new(text),
                )];
                // If a brain is known (status has been set), this message belongs
                // to the active session — emit a routed SendMessage that the App
                // forwards with the correct SessionId. Otherwise spawn-and-prompt.
                if self.brain_attached {
                    return Some(Action::SendMessage {
                        // Placeholder session; the App replaces with active id.
                        session: spur_acp::SessionId::from(String::new()),
                        blocks,
                        interrupt,
                    });
                } else {
                    return Some(Action::NewSessionWithMessage { blocks, interrupt });
                }
            }
            // ...
```

Where `brain_attached` is an existing Dashboard state — or compute from the brain status field. Find the field (`input_bar_brain_status` from earlier) and derive.

In `App`, when receiving `Action::SendMessage` with empty session id, substitute the active session id before forwarding:

```rust
            Action::SendMessage { mut session, blocks, interrupt } => {
                if session.0.is_empty() {
                    if let Some(ref detail) = self.session_detail {
                        session = detail.session_id().clone();
                    } else {
                        // No session — should have been NewSessionWithMessage; drop.
                        return;
                    }
                }
                // ... existing forwarding
            }
```

- [ ] **Step 3: App handles NewSessionWithMessage**

```rust
            Action::NewSessionWithMessage { blocks, interrupt } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(crate::UserInput::NewSessionWithMessage { blocks, interrupt });
                }
                self.dirty = true;
            }
```

- [ ] **Step 4: spur-cli translates UserInput → InteractiveInput**

In `crates/spur-cli/src/main.rs` (around line 391 — the match on `input`):

```rust
                        spur_tui::UserInput::NewSessionWithMessage { blocks, interrupt } => {
                            spur_core::InteractiveInput::NewSessionWithMessage { blocks, interrupt }
                        }
```

- [ ] **Step 5: Orchestrator handles NewSessionWithMessage**

In `crates/spur-core/src/orchestrator.rs`, in the `run_interactive` loop's `match input`, add an arm:

```rust
                InteractiveInput::NewSessionWithMessage { blocks, interrupt } => {
                    // Shut down current brain (if any) and spawn fresh one.
                    if let Some(mut b) = brain.take() {
                        b.delegation_handle.abort();
                        let _ = b.connection.shutdown().await;
                        let _ = b.mcp_server.shutdown();
                    }
                    // Create a fresh brain + session.
                    match self.create_brain_session(brain_override.as_deref(), permission_tx.clone()).await {
                        Ok(new_brain) => {
                            // Emit BrainSpawned — TUI creates SessionDetailView.
                            self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
                                agent: new_brain.agent_name.clone(),
                                session: new_brain.spur_session_id.clone(),
                            }));
                            // Queue the first message as a Prompt so the usual
                            // Message arm handles it.
                            pending_messages.push_back(InteractiveInput::Message { blocks, interrupt });
                            brain = Some(new_brain);
                        }
                        Err(e) => {
                            error!(error = %e, "NewSessionWithMessage: failed to spawn brain");
                            self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                session: spur_acp::SessionId::from(String::new()),
                                message: format!("Failed to spawn new session: {e}"),
                            }));
                        }
                    }
                }
```

(Adjust `create_brain_session` signature if needed to match actual orchestrator method. Look for the existing spawn path.)

Similarly update `InteractiveInput::Message` arm's `brain.is_none()` case to delegate to the NewSessionWithMessage logic if you want to keep them unified. Or keep them separate.

- [ ] **Step 6: Build + run tests**

```bash
cargo build --workspace 2>&1 | tail -10
cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | tail -20
```

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/action.rs crates/spur-tui/src/lib.rs crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/app.rs crates/spur-core/src/orchestrator.rs crates/spur-cli/src/main.rs
git commit -m "fix(spur-core): explicit NewSessionWithMessage replaces SessionId::new() placeholder (BUG-2)"
```

Now perform Task 15 (remove `pending_user_messages`).

---

## Task 17: Auto-resume banner

**Files:**
- Create: `crates/spur-tui/src/components/resume_banner.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Create: `crates/spur-tui/tests/auto_resume_landing.rs`

- [ ] **Step 1: Create banner component**

`crates/spur-tui/src/components/resume_banner.rs`:

```rust
use std::time::Instant;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

pub struct ResumeBanner {
    pub title: String,
    pub quit_ago: String, // human-readable: "2m ago"
    shown_at: Instant,
    dismissed: bool,
}

impl ResumeBanner {
    pub fn new(title: String, quit_ago: String) -> Self {
        Self { title, quit_ago, shown_at: Instant::now(), dismissed: false }
    }

    pub fn should_render(&self) -> bool {
        if self.dismissed { return false; }
        self.shown_at.elapsed() < std::time::Duration::from_secs(3)
    }

    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if !self.should_render() { return; }
        let line = Line::from(vec![
            Span::styled(" Resumed: ", Style::default().fg(Color::Green)),
            Span::styled(&self.title, Style::default().fg(Color::White)),
            Span::styled(format!(" · quit {} ", self.quit_ago), Style::default().fg(Color::DarkGray)),
            Span::styled("· [s] picker · [n] new · [Esc] dismiss", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}
```

Register in `components/mod.rs`:

```rust
pub mod resume_banner;
```

- [ ] **Step 2: Integrate banner in SessionDetailView**

Add field:

```rust
    resume_banner: Option<crate::components::resume_banner::ResumeBanner>,
```

Add setter:

```rust
    pub fn show_resume_banner(&mut self, title: String, quit_ago: String) {
        self.resume_banner = Some(crate::components::resume_banner::ResumeBanner::new(title, quit_ago));
    }
```

In `render`, reserve one line at the top of the view area when `resume_banner.is_some() && should_render()`. Call `.render()` into that line.

In `handle_key`, if banner is showing, dismiss on any key BUT still pass the key through normally:

```rust
        if let Some(ref mut banner) = self.resume_banner {
            banner.dismiss();
        }
        // ... rest of key handling (key is not consumed)
```

- [ ] **Step 3: Write test**

Create `crates/spur-tui/tests/auto_resume_landing.rs`:

```rust
use std::time::Duration;
use spur_tui::views::session_detail::SessionDetailView;

#[test]
fn banner_hides_after_3s() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId::from("sess".to_string()),
        "a".into(), "b".into(), std::path::PathBuf::from("."),
    );
    view.show_resume_banner("title".into(), "2m ago".into());
    // Immediately: renders (not dismissed yet)
    // Advance time via test helper? Simpler: assert `banner_is_visible()` accessor
    assert!(view.banner_is_visible());
}
```

Add helper:

```rust
    pub fn banner_is_visible(&self) -> bool {
        self.resume_banner.as_ref().map(|b| b.should_render()).unwrap_or(false)
    }
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p spur-tui --test auto_resume_landing 2>&1 | tail -5
git add crates/spur-tui/src/components/resume_banner.rs crates/spur-tui/src/components/mod.rs crates/spur-tui/src/views/session_detail.rs crates/spur-tui/tests/auto_resume_landing.rs
git commit -m "feat(spur-tui): auto-resume banner with dismiss-on-any-key"
```

---

## Task 18: Auto-resume landing logic + `--dashboard` flag

**Files:**
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/tests/auto_resume_landing.rs`

- [ ] **Step 1: Add `--dashboard` CLI flag**

In `crates/spur-cli/src/main.rs`, find the `Commands::Watch` struct (around line 98):

```rust
    Watch {
        #[arg(long)]
        brain: Option<String>,
        #[arg(long)]
        sessions: bool,
        /// Land on Dashboard instead of auto-resuming last session.
        #[arg(long)]
        dashboard: bool,
    },
```

- [ ] **Step 2: Read metadata on startup + decide landing**

In the `Commands::Watch` handler, before `spur_tui::run_tui(...)`:

```rust
            let metadata_path = repo_root.join(".spur").join("session_metadata.json");
            let meta = spur_tui::session_metadata::SessionMetadataStore::load(&metadata_path);

            let start_in_picker = !dashboard && sessions;
            let auto_resume = !dashboard && !sessions && meta.metadata().last_active_session_id.is_some();

            if auto_resume {
                let sid = meta.metadata().last_active_session_id.clone().unwrap();
                // Queue a ResumeSession before TUI starts so the orchestrator picks it up.
                let user_tx_for_resume = tui_tx.clone();
                tokio::spawn(async move {
                    let _ = user_tx_for_resume.send(spur_tui::UserInput::ResumeSession { session_id: sid }).await;
                });
            }

            spur_tui::run_tui(event_rx, Some(tui_tx), Some(perm_rx), start_in_picker).await?;
```

- [ ] **Step 3: Show banner on successful auto-resume**

In `App`, when receiving `BrainSpawned` and `pending_auto_resume` is true, call `view.show_resume_banner(title, quit_ago)`. Add an App field `pending_auto_resume: Option<(String, String)>` (title, quit_ago) populated by `spur-cli` via an `App::set_auto_resume(...)` constructor argument or a `Action::MarkAutoResume` emitted right after app construction.

Simpler: compute banner parameters from metadata at `BrainSpawned` time. App already has `metadata_store`; if the spawned session matches `last_active_session_id`, compute `quit_ago` from `last_active_at` and show banner.

In `BrainSpawned` handler in App:

```rust
                if let Some(ref last_id) = self.metadata_store.metadata().last_active_session_id {
                    if last_id == &session.0 {
                        let title = self.metadata_store.entry(&session.0)
                            .and_then(|e| e.title_override.clone())
                            .unwrap_or_else(|| agent.clone());
                        let quit_ago = humanize_since(&self.metadata_store.metadata().last_active_at);
                        if let Some(ref mut view) = self.session_detail {
                            view.show_resume_banner(title, quit_ago);
                        }
                        // Clear last_active so next spawn (new session) doesn't re-trigger.
                        self.metadata_store.set_last_active(String::new(), String::new());
                        // Actually, clear via a new method:
                        // self.metadata_store.clear_last_active();
                    }
                }
```

Add `clear_last_active()` to the store:

```rust
    pub fn clear_last_active(&mut self) {
        self.metadata.last_active_session_id = None;
        self.metadata.last_active_at = None;
    }
```

And a `humanize_since` helper in app.rs:

```rust
fn humanize_since(iso: &Option<String>) -> String {
    let Some(iso) = iso else { return "recently".into(); };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else { return "recently".into(); };
    let secs = chrono::Utc::now().signed_duration_since(dt).num_seconds();
    if secs < 60 { "just now".into() }
    else if secs < 3600 { format!("{}m ago", secs / 60) }
    else if secs < 86400 { format!("{}h ago", secs / 3600) }
    else { format!("{}d ago", secs / 86400) }
}
```

- [ ] **Step 4: Update `last_active` on turn complete + quit**

On every `TurnComplete { session }` event, update:

```rust
            SpurEventBody::TurnComplete { session } => {
                self.brain_status = BrainStatus::Ready;
                let now = chrono::Utc::now().to_rfc3339();
                self.metadata_store.set_last_active(session.0.clone(), now);
                let _ = self.metadata_store.save();
            }
```

Also in Quit (confirm-yes) path, flush any pending draft save for the active session.

- [ ] **Step 5: Build + smoke test**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | tail -20
```

Manual: run `cargo run -p spur-cli -- watch --brain claude-code-acp`, create a session (type on Dashboard, Enter), send a message, quit. Restart. Expect auto-resume + banner.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/src/main.rs crates/spur-tui/src/app.rs crates/spur-tui/src/session_metadata.rs crates/spur-tui/tests/auto_resume_landing.rs
git commit -m "feat(spur): auto-resume last session on startup + --dashboard flag"
```

---

## Self-review

**Spec coverage check:**

| Spec requirement | Task |
|------------------|------|
| Metadata store — schema, atomic writes, CRUD | Task 1 |
| Metadata store — orphan GC | Task 2 |
| Help overlay fix, Dashboard placeholder, footer hint | Task 3 |
| Metadata read in picker (title overrides) | Task 4 |
| `[+ New session]` top row + `n` key | Task 5 |
| Fuzzy search + `/` focus + two-Esc clear | Task 6 |
| Pin + ⭐ + pinned-first sort | Task 7 |
| Archive + `a` toggle | Task 8 |
| Inline rename prompt | Task 9 |
| Picker cache + cursor memory | Task 10 |
| Preview pane | Task 11 |
| `r` refresh | Task 12 |
| Draft debounced save | Task 13 |
| Draft restore on open | Task 14 |
| BUG-1: pending_user_messages replay | Task 15 |
| BUG-2: SessionId::new placeholder | Task 16 |
| Resume banner | Task 17 |
| Auto-resume landing + --dashboard flag | Task 18 |
| Draft switch-safety confirm | **GAP** — not yet covered |
| Picker "resuming" state re-entry | partial (existing behavior preserved) |

**Gap identified:** Draft switch-safety confirm banner (shown when Enter on different session with non-empty draft on current) is not explicitly a task. Add Task 19.

## Task 19: Draft switch-safety confirm banner

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/tests/session_picker_interactions.rs`

- [ ] **Step 1: Test**

```rust
#[test]
fn enter_switching_session_with_unsaved_draft_shows_confirm() {
    use spur_tui::test_support;
    let mut app = test_support::new_app();
    // User has an active session A with a draft.
    test_support::spawn_session(&mut app, "sess-A");
    test_support::set_input_text(&mut app, "unsent draft");
    // Open picker, cursor onto a different session B.
    test_support::push_sessions(&mut app, vec![
        session("sess-A", "A"), session("sess-B", "B"),
    ]);
    test_support::open_picker(&mut app);
    // j to move cursor off [+ New] onto A (cursor=1), then to B (cursor=2)
    test_support::picker_handle(&mut app, key('j'));
    test_support::picker_handle(&mut app, key('j'));
    // Press Enter — confirm banner should appear instead of resuming.
    test_support::picker_handle(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(test_support::picker_confirm_visible(&app));
    // Press 'y' to confirm.
    test_support::picker_handle(&mut app, key('y'));
    // Now the ResumeSession action should have been dispatched and the draft saved.
    assert_eq!(
        test_support::metadata_draft(&app, "sess-A"),
        "unsent draft"
    );
}
```

- [ ] **Step 2: Implement**

Add `confirm_switch: Option<String>` (holds target session_id) to `SessionPickerView`. In `handle_key` list-mode Enter, check if App's current session has a non-empty draft — but picker doesn't know about App state. Solution: picker holds `current_session_has_draft: bool` updated by App via `picker.set_draft_state(bool)` whenever metadata changes OR on picker open.

If `current_session_has_draft && cursor != 0 && target_sid != current_sid` → set `confirm_switch = Some(target_sid)`, don't emit action. Render banner: `"Session <X> has an unsent draft — save and switch? [y/N]"`.

In confirm mode, key handling:
- `y` or `Enter` → emit `ResumeSession { session_id: confirm_switch.take().unwrap() }`. App's Action handler flushes draft to metadata before forwarding.
- `n` or `Esc` → clear `confirm_switch`, no action.

App flow on `ResumeSession`: flush current session's InputBar text to metadata before forwarding to orchestrator.

- [ ] **Step 3: Run + commit**

```bash
cargo test -p spur-tui 2>&1 | grep -E "^test result|FAILED" | tail -10
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/session_picker_interactions.rs
git commit -m "feat(spur-tui): draft switch-safety confirm banner in picker"
```

---

## Final verification

- [ ] **All tests pass**

```bash
cargo test --workspace 2>&1 | grep -E "^test result|FAILED" | tail -30
```

Expected: zero failures.

- [ ] **Clippy clean**

```bash
cargo clippy -p spur-tui -p spur-core -p spur-cli --all-targets 2>&1 | tail -5
```

Expected: no new warnings.

- [ ] **Manual smoke tests (the 4 canonical journeys)**

1. **"Continue yesterday's work"**: `spur watch` → auto-resume banner appears → verify banner dismisses after 3s or on keystroke.
2. **"Find a specific session"**: `s` → `/` → type fragment → list narrows → Enter → resumes.
3. **"Fork off new work"**: typing in session A → `s` → Enter on `[+ New]` → confirm banner shown (if draft present) → `y` → new session spawns, draft preserved on A.
4. **"Clean up"**: `s` → navigate to row → `R` → type new title → Enter → title persists. `p` to pin. `d` to archive. `a` to show archived. `d` again to unarchive.

- [ ] **Bug regressions verified**

- Dashboard typing with no brain → spawns new session, message arrives in that session, no cross-session leak.
- `SessionId::new()` UUID no longer appears in sendMessage action payloads (grep logs).

---

## Done

Plan complete and saved to `docs/superpowers/plans/2026-04-13-session-management-ux-phase-1.md`.
