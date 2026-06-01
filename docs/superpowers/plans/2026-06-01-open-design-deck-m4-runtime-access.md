# Open Design on Jute — M4 Runtime Access Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILLS: `plan-task-discipline` (DAG order, file-scope isolation),
> `test-driven-development` (red→green where stated), `verification-before-completion` (run the gate before
> claiming done), `rust-idioms`. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Give the `open-design` skill a **path-independent runtime access surface** for the two
already-vendored libraries (148 design-systems, 51 deck-themes) so it works in a packaged Jute.app, not
just an in-repo checkout. Add a Rust loader + `open_design_*` MCP tools, bundle the assets as Tauri
resources, and rewire the skill to call the tools (with `Read` kept as a dev fallback).

**Reference spec:** `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m4-runtime-access-design.ipynb`.
**This plan is authoritative over the spec** on file/schema specifics below (the spec predates two verified
facts: the deck index schema differs from design-systems, and `example.html` is present in only 49/51 themes).

## Locked decisions for M4 (recommended defaults from the spec, settled)

1. **Install model:** read-in-place from the resolved root — **no copy**. `~/.spur/open-design/<kind>/` is an
   *optional, possibly-absent* user overlay, never seeded by us.
2. **Tools:** ship `open_design_search` + `open_design_get`. `open_design_list` and MCP Resources are **deferred** (M4+).
3. **Server placement:** add the tools to the **existing notebook MCP server** (shares `ServerDeps`/`resource_dir`).
4. **Naming:** `open_design_*` (underscore). Verified collision-free — the only `open_design*` symbols in the
   tree are `spur-core` test fns. No `opendesign://` Resource scheme in M4 (deferred).
5. **Ranking:** deterministic field-weighted token score. No embeddings.

## Verified codebase facts the implementation depends on

- MCP tool = `pub fn tool() -> Tool` (inline JSON schema) + `pub async fn call(deps: &ServerDeps, arguments: Value)
  -> Result<CallToolResult, McpError>`. Register the `tool()` in `crates/spur-notebook/src/mcp/tools/mod.rs::tools()`
  (`fn tools()`), and add a dispatch arm in `impl ServerHandler for NotebookMcpServer::call_tool`
  (`crates/spur-notebook/src/mcp/mod.rs`). Representative tool to copy: `src/mcp/tools/add_api_datasource.rs`.
- `ServerDeps` (`src/mcp/mod.rs`) carries `app: Option<tauri::AppHandle>`. `app.path().resource_dir()` is the
  bundle-resource locator. When `app` is `None` (headless/tests), fall through to env/repo resolution.
- Install precedent to mirror in shape + test style: `src/extension_install.rs`
  (`BaseDirs` for `~/.spur`, `$HOME` fallback, tempdir tests). M4 reads in place — it does NOT copy — but the
  resolution/test shape is the model.

## Asset layout (ground truth — measured)

```
crates/spur-notebook/assets/open-design-library/            # kind = design-systems
├── index.json            # { version, kind:"design-systems", count:148, items:[{id,title,category,summary,swatches[]}] }
└── design-systems/<id>/DESIGN.md                            # 148; bmw-m has empty swatches

crates/spur-notebook/assets/open-design-deck-library/       # kind = deck-themes
├── index.json            # { version, kind:"deck-themes", count:51, items:[{id,title,scenario,mode,featured,summary,source,swatches[]}] }
├── deck-skeleton.html    # shared 1920×1080 framework
└── deck-themes/<id>/                                        # 51
      ├── SKILL.md         # ALWAYS present (51/51)
      ├── example.html     # OPTIONAL — present in 49/51 (guizang-ppt + 1 lack it)
      ├── assets/ references/ README*.md LICENSE              # side files (vary)
```

**Two index schemas differ:** design-systems items have `category`; deck-themes items have `scenario` +
`mode` + `featured` + `source`. Both have `id`, `title`, `summary`, `swatches`. The loader must parse both
tolerantly and project them onto one ranked shape.

---

## Task 1 — `open_design` loader module (no MCP yet)

**Files (create unless noted):**
- `crates/spur-notebook/src/open_design/mod.rs`
- `crates/spur-notebook/src/open_design/library.rs`
- `crates/spur-notebook/src/lib.rs` — **modify**: add `pub mod open_design;`

**Contract (`library.rs`):**

```rust
pub enum Kind { DesignSystems, DeckThemes }
impl Kind {
    pub fn lib_dir(self) -> &'static str;   // "open-design-library" | "open-design-deck-library"
    pub fn sub_dir(self) -> &'static str;   // "design-systems"      | "deck-themes"
    pub fn as_str(self) -> &'static str;     // "design-systems"      | "deck-themes" (for JSON `kind`)
    pub fn parse(s: &str) -> Option<Kind>;
}

#[derive(serde::Deserialize)]   // tolerant: every non-id/title field is optional
pub struct IndexItem {
    pub id: String,
    pub title: String,
    #[serde(default)] pub category: Option<String>,   // design-systems
    #[serde(default)] pub scenario: Option<String>,   // deck-themes
    #[serde(default)] pub summary: Option<String>,
    #[serde(default)] pub swatches: Vec<String>,
}
pub struct Ranked {           // unified search row
    pub id: String, pub kind: String, pub title: String,
    pub category: Option<String>,   // category OR scenario, whichever present
    pub summary: Option<String>, pub swatches: Vec<String>, pub score: f64,
}
pub struct DeckTheme {
    pub id: String,
    pub skill_md: String,                 // required
    pub example_html: Option<String>,     // 49/51 — MUST be Option
    pub deck_skeleton_html: Option<String>, // only when include_skeleton
    pub files: Vec<FileEntry>,            // { path: String (relative), bytes: u64 } manifest of side files
}

/// Resolution order, first existing wins. `resource_dir` = deps.app.path().resource_dir() or None.
pub fn resolve_root(kind: Kind, resource_dir: Option<&std::path::Path>) -> Option<std::path::PathBuf>;
//   1. $SPUR_OPEN_DESIGN_LIBRARY/<lib_dir>            (env override; dev/tests)
//   2. ~/.spur/open-design/<lib_dir>                  (user overlay, only if it exists & contains index.json)
//   3. <resource_dir>/<lib_dir>                       (shipped bundle)
//   4. <CARGO_MANIFEST_DIR>/assets/<lib_dir>          (repo-relative dev fallback)
//   A "root" is the dir that directly contains index.json.

pub fn load_index(kind: Kind, root: &Path) -> Result<Vec<IndexItem>, LibraryError>;
pub fn get_design_system(root: &Path, id: &str) -> Result<String, LibraryError>;   // design-systems/<id>/DESIGN.md
pub fn get_deck_theme(root: &Path, id: &str, include_skeleton: bool) -> Result<DeckTheme, LibraryError>;
pub fn search(query: &str, kind: Option<Kind>, limit: usize,
              resource_dir: Option<&Path>) -> Result<Vec<Ranked>, LibraryError>;

pub enum LibraryError { RootNotFound(Kind), NotFound { kind: String, id: String }, Io(std::io::Error), Json(serde_json::Error) }
```

**Ranking (deterministic):** lowercase + split `query` on non-alphanumeric into tokens. Per item:
`+3` if a token equals a token of `id` or `title`; `+2` if a token equals a token of `category`/`scenario`;
`+1` substring hit in `title`/`summary`. If a token is a `#hex` or matches a swatch substring, `+2` per swatch
match. Drop score-0 items **only when query is non-empty**; empty query returns all (acts as list). Sort by
`(score desc, id asc)`; truncate to `limit` (default 8 in the tool, not the lib).

- [ ] **Step 1:** Write `mod.rs` (`pub mod library; pub use library::*;` + `Kind`/`LibraryError` may live here or in `library.rs`, your call) and the full `library.rs` per the contract.
- [ ] **Step 2:** Add `pub mod open_design;` to `crates/spur-notebook/src/lib.rs` (alphabetical with siblings).
- [ ] **Step 3 (TDD):** Add a `#[cfg(test)] mod tests` in `library.rs` using `tempfile` (already a dev-dep — mirror `extension_install.rs` tests). Cover, each as its own `#[test]`:
  - `resolve_root_prefers_env_override_then_resource_then_repo` — build temp dirs, set/unset `SPUR_OPEN_DESIGN_LIBRARY` via a guard, assert precedence (env > resource > repo). **Do not** depend on a real `~/.spur`.
  - `resolve_root_returns_none_when_nothing_exists`.
  - `load_index_parses_both_schemas` — feed a design-systems fixture (with `category`) and a deck-themes fixture (with `scenario`); assert both parse and `swatches` round-trip.
  - `get_deck_theme_tolerates_missing_example_html` — theme dir with `SKILL.md` only → `example_html == None`, `skill_md` populated. **This is the 2-of-51 edge case; it MUST pass.**
  - `get_design_system_missing_id_errors` → `LibraryError::NotFound`.
  - `search_ranks_title_match_above_summary_only` and `search_empty_query_returns_all`.
- [ ] **Step 4:** Run the gate. **GATE:** `cargo test -p spur-notebook open_design` is green (and `cargo build -p spur-notebook` clean). Commit once.

**Out of scope for T1:** any MCP tool, any `tauri.conf.json` edit, touching `spur-core`.

---

## Task 2 — `open_design_search` + `open_design_get` MCP tools  (depends_on: T1)

**Files (create unless noted):**
- `crates/spur-notebook/src/mcp/tools/open_design_search.rs`
- `crates/spur-notebook/src/mcp/tools/open_design_get.rs`
- `crates/spur-notebook/src/mcp/tools/mod.rs` — **modify**: declare the two modules; add their `tool()` to `fn tools()`.
- `crates/spur-notebook/src/mcp/mod.rs` — **modify**: add two arms to `ServerHandler::call_tool`.

**Copy the shape of `add_api_datasource.rs` exactly** (`#[derive(Deserialize)]` params struct → `tool()` with
inline JSON schema → `async fn call` deserializing params and returning `CallToolResult::structured(json!({…}))`).
Resolve `resource_dir` from `deps.app.as_ref().and_then(|a| a.path().resource_dir().ok())` and pass it into the
`library::*` calls. Map `LibraryError::RootNotFound`/`NotFound` to `McpError::invalid_params`, `Io`/`Json` to
`McpError::internal_error`.

- `open_design_search`: params `{ query: String, kind: Option<String>, limit: Option<usize> }` (kind parsed via
  `Kind::parse`; `None`/absent = search both and merge). Returns `{ "items": [Ranked…] }` (serialize `Ranked`).
- `open_design_get`: params `{ kind: String, id: String, include_skeleton: Option<bool> }`. For
  `design-systems` → `{ id, kind, design_md }`. For `deck-themes` → `{ id, kind, skill_md, example_html?,
  deck_skeleton_html?, files: [{path,bytes}] }` (omit/null `example_html` when absent — do not error).

- [ ] **Step 1:** Implement both tool files.
- [ ] **Step 2:** Register in `tools/mod.rs::tools()` and add the two `call_tool` match arms in `mcp/mod.rs`
  (match the exact tool-name strings `"open_design_search"` / `"open_design_get"`).
- [ ] **Step 3 (test):** Add a test asserting `tools()` now contains both names (mirror however existing tool
  registration is tested; if none, assert on the `Vec<Tool>` names). A behavioral test that calls `call` with a
  fixture root via `SPUR_OPEN_DESIGN_LIBRARY` is a bonus, not required.
- [ ] **Step 4:** **GATE:** `cargo test -p spur-notebook` green; `cargo clippy -p spur-notebook --all-targets`
  clean (no new warnings). Commit once.

---

## Task 3 — Bundle the asset dirs as Tauri resources  (no deps)

**File:** `crates/spur-notebook/tauri.conf.json` — **modify** only.

- [ ] **Step 1:** Add a `bundle.resources` map (the key does not exist yet) so the two dirs land under
  `resource_dir()`:
  ```json
  "resources": {
    "assets/open-design-library": "open-design-library",
    "assets/open-design-deck-library": "open-design-deck-library"
  }
  ```
  Preserve every existing `bundle` field (`active`, `targets`, `icon`, `externalBin`, `fileAssociations`).
- [ ] **Step 2:** **GATE:** `python3 -c "import json; json.load(open('crates/spur-notebook/tauri.conf.json'))"`
  exits 0 (valid JSON) **and** `cargo check -p spur-notebook` is clean. Commit once.

> Note: paths are relative to `tauri.conf.json`'s dir. If the bundler rejects a glob/dir form during a real
> `tauri build`, that's an M4 follow-up — this task only lands the declared resource mapping + valid config.

---

## Task 4 — Rewire the `open-design` skill to the tools  (depends_on: T2)

**Files (modify):**
- `crates/spur-core/src/skills/open-design/references/design-systems.md`
- `crates/spur-core/src/skills/open-design/references/deck-artifact.md`
- `crates/spur-core/src/skills/open-design/SKILL.md`
- `crates/spur-core/src/skills/mod.rs` — extend the existing assertions.

- [ ] **Step 1 (TDD):** In `crates/spur-core/src/skills/mod.rs` `tests`, extend (or add beside) the existing
  `open_design_references_design_system_library` test so it asserts `design-systems.md` mentions
  `open_design_search` and `open_design_get`; add an analogous assertion that `deck-artifact.md` mentions them.
  Run it → **RED**.
- [ ] **Step 2:** Update `references/design-systems.md`: make the primary selection path *call
  `open_design_search({query, kind:"design-systems"})` then `open_design_get({kind:"design-systems", id})`*;
  keep the existing `Read assets/...` line explicitly labelled as a **dev fallback**.
- [ ] **Step 3:** Update `references/deck-artifact.md` the same way for `kind:"deck-themes"`, fetching the
  skeleton via `open_design_get({…, include_skeleton:true})`. Keep the `Read` fallback note.
- [ ] **Step 4:** Add `open_design_search` / `open_design_get` to the tool roster in `SKILL.md`'s `<HARD-GATE>`.
- [ ] **Step 5:** **GATE:** `cargo test -p spur-core --lib skills` fully green (the new assertions + all
  pre-existing open-design tests). Commit once.

**Do NOT** edit `CREATION-LOG.md` here (that is Task 5). Do not touch `spur-notebook`.

---

## Task 5 — Provenance  (depends_on: T4)

**File:** `crates/spur-core/src/skills/open-design/CREATION-LOG.md` — **modify** only.

- [ ] **Step 1:** Append the M4 entry (verbatim):
  ```markdown

  - **2026-06-01** — M4: runtime access surface. Added a `crates/spur-notebook/src/open_design/` loader
    (resolution order: `$SPUR_OPEN_DESIGN_LIBRARY` → `~/.spur/open-design/` overlay → Tauri `resource_dir()`
    → repo `assets/`; read-in-place, no copy) and two MCP tools — `open_design_search` (deterministic
    field-weighted ranking over both libraries) and `open_design_get` (path-independent package fetch;
    `example_html` optional, present in 49/51 deck themes). Bundled both asset dirs as Tauri resources and
    rewired the skill's Direction / artifact-deck steps to call the tools (`Read` kept as dev fallback).
    `open_design_list` + MCP Resources deferred to M4+. Spec:
    `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m4-runtime-access-design.ipynb`.
  ```
- [ ] **Step 2:** **GATE:** `cargo test -p spur-core --lib skills` green. Commit once.

---

## Self-review notes
- **File-scope isolation:** T1 (`spur-notebook/src/open_design/*` + `lib.rs`), T2 (`spur-notebook/src/mcp/*`),
  T3 (`tauri.conf.json`), T4 (`spur-core` skills), T5 (`CREATION-LOG.md`) touch disjoint files. T1∥T3 parallel.
- **DAG:** T2←T1 (uses `library::*`); T4←T2 (skill names tools that must exist); T5←T4 (records completed work).
- **The two correctness traps are encoded as required tests:** dual index schema (`load_index_parses_both_schemas`)
  and optional `example.html` (`get_deck_theme_tolerates_missing_example_html`).
- **No new deps:** `serde`/`serde_json`/`tempfile`/`directories` are already in `spur-notebook`.
