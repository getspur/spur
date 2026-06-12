# AI Sidebar Context Lenses - Design Spec

- **Status:** Draft for user review
- **Date:** 2026-06-12
- **Surface:** `crates/spur-notebook/jute-notebook/src/ui/notebook/sidebar/ChatPanel.tsx`
- **Related:** `2026-06-09-notebook-sidebar-ai-agent-design.md`, `2026-06-12-ai-sidebar-single-tab-chat-panel-design.md`
- **Design Epic:** `bd-f1ab`
- **Review Source:** `bd-1t1s`, `claude-code` delegation `96639037-4323-4d54-be1a-e01373572fe4`

## 1. Goal

Add an explicit context and lens model to the AI sidebar so the assistant can
respond from the user's current notebook perspective without fragmenting the
single chat panel.

The user can be looking at the same notebook through different surfaces:

- notebook cells
- DAG execution graph
- rendered app/product view

Those surfaces need different assistant posture. The first implementation should
therefore preserve the single-tab chat panel while adding compact context
framing for each turn.

## 2. Core Decision

Separate three concepts that currently risk being blended together:

| Concept | Owner | Lifetime | Purpose |
|---|---|---|---|
| Scope | Backend/resolved app scope | Session-level | Determines cwd, app key, MCP tools, skill, and session identity |
| View mode | Notebook UI state | UI-level | Describes what the user is currently looking at: notebook, DAG, or app |
| Lens | User/heuristic assistant posture | Turn-level | Describes how the assistant should frame the next answer |

The important rule is:

**Lens is turn-level context, not session identity.**

Switching from `notebook_builder` to `notebook_deep_dive` should not create a
new ACP session. The same session can contain turns from different lenses. Scope
changes can create or resume sessions; lens changes should only alter prompt
framing for the next turn.

## 3. Lens Model

Use a flat lens enum for the first pass:

```ts
type NotebookViewMode = "notebook" | "dag" | "app";

type ChatLens =
  | "notebook_builder"
  | "notebook_deep_dive"
  | "dag_ops"
  | "app_product";

type ChatTurnContext = {
  notebookPath: string;
  viewMode: NotebookViewMode;
  lens: ChatLens;
};
```

Keep `scopeKey`, `scopeLabel`, `appRoot`, and `appName` in the UI/backend scope
model rather than in the per-turn lens contract. They are still needed, but they
belong to session scope, not to the lens.

## 4. Default Lens Mapping

The sidebar should derive a default lens from the current `viewMode`, then allow
the user to override it where useful.

| View mode | Default lens | Alternate lens | Assistant posture |
|---|---|---|---|
| `notebook` | `notebook_builder` | `notebook_deep_dive` | Grow, improve, explain, and restructure the notebook |
| `dag` | `dag_ops` | none in first pass | Operate/debug dependency graph execution |
| `app` | `app_product` | none in first pass | Review and improve the app as a product/interface |

### Notebook Builder

Builder mode helps users grow and enhance the notebook:

- propose next cells
- improve analysis flow
- draft code or markdown
- restructure rough exploration into a stronger artifact
- identify missing data checks, visualizations, or explanations

### Notebook Deep Dive

Deep dive mode helps users understand the notebook:

- explain what the notebook is doing
- summarize cell purpose and data flow
- explain outputs and assumptions
- identify how pieces connect
- answer "how does this work?" questions

### DAG Operations

DAG mode is operational:

- explain failed, stale, blocked, or expensive nodes
- reason about recomputation order
- identify dependency impact
- help users recover execution health
- summarize pipeline state

Future work may split this into `dag_ops` and `dag_explain`, but one lens is
enough for the first implementation.

### App Product

App mode is product-focused:

- review the rendered app as user interface
- improve product workflow, copy, layout, and interaction quality
- reason about product behavior
- suggest product/UI changes instead of notebook-internal changes

If `viewMode === "app"` but no `appOpenInfo` exists, copy should soften to
"current app view" or fall back to notebook deep-dive behavior. The header can
still show the app lens, but prompt framing must not claim app tools exist when
they do not.

## 5. UX Design

The first implementation remains a single chat panel.

Do not introduce multi-tab sessions for this slice. The user-approved direction
is a single-tab panel with stronger context. Tabs can come later when transcript
management itself becomes the bottleneck.

### Header

Add a compact lens control near the existing scope/status area:

```text
AI Agent
Notebook: analysis.ipynb               Ready
[Builder] [Deep dive]
```

For DAG/app modes, render a single compact indicator rather than a choice:

```text
DAG: Operations
App: Product
```

The control should be visually secondary to the scope label. It is an assistant
posture selector, not navigation.

### Empty State

Empty-state copy should reflect the selected lens:

| Lens | Empty-state heading | Supporting copy |
|---|---|---|
| `notebook_builder` | Build on this notebook | Ask for the next cell, a cleaner analysis path, or stronger explanation. |
| `notebook_deep_dive` | Understand this notebook | Ask how the cells, outputs, and assumptions fit together. |
| `dag_ops` | Operate this graph | Ask about failed nodes, stale dependencies, or recomputation order. |
| `app_product` | Improve this app | Ask about workflow, UI quality, copy, or product behavior. |

### Composer

Composer microcopy should include the active lens:

```text
Ready in Revenue Dashboard - Product lens
Ready in analysis.ipynb - Builder lens
```

Changing lens applies to the next submitted turn. It does not rewrite existing
messages and does not reload sessions.

## 6. Backend Contract

The minimal backend change is to pass explicit turn context to `chat_turn`.

```ts
invoke("chat_turn", {
  agentName,
  notebookPath,
  prompt,
  context: {
    notebookPath,
    viewMode,
    lens,
  },
  onEvent,
});
```

Rust should define a serde-compatible payload:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnContext {
    pub notebook_path: String,
    pub view_mode: NotebookViewMode,
    pub lens: ChatLens,
}
```

The backend still resolves authoritative scope from the notebook path/app
manifest for session identity. The context payload is used for turn framing,
not for deciding session storage.

### Prompt Framing

The backend should prepend a short lens preamble to the turn prompt. Keep it
short and deterministic.

Example:

```text
Current user perspective: Notebook builder.
Help the user grow and improve this notebook. Prefer concrete next cells,
analysis structure, and executable edits when appropriate.
```

Start by injecting the preamble on every turn. If agents visibly echo it or token
cost becomes meaningful, dedupe when the lens has not changed.

## 7. Scope Resolution

This spec does not replace backend scope resolution.

The existing backend still owns:

- app root detection
- `spur-app.json` manifest loading
- app key construction
- MCP server list
- skill loading
- ACP session creation/resume

However, frontend and backend scope keys should not diverge. The current
frontend duplicates part of backend scope identity when it constructs
`chatScopeKey`. A later slice should add a `chat_resolve_scope` command so the
frontend can display the same scope key and label the backend will use.

That is not required for the first lens slice, but tests should document the
current parity.

## 8. State Rules

- Scope changes can create/resume a session.
- View mode changes update the default lens.
- If the user has manually selected a lens, reset the override when `viewMode`
  changes.
- Lens changes do not call `chat_new_session`.
- Lens changes do not clear transcript history.
- Submitted turns capture `viewMode` and `lens` at submit time, just like the
  existing code captures notebook path, scope key, and agent name.

## 9. Non-Goals

- Multi-tab chat sessions.
- Separate ACP sessions per lens.
- Persisted per-message lens badges in the first pass.
- App-defined lens prompt overrides.
- A full custom select/dropdown framework.
- Replacing the existing `NotebookSidebar` shell.
- Rewriting session listing or ACP replay behavior.

## 10. Implementation Boundaries

The first implementation should be split into small, testable slices:

1. **Frontend lens state and UI**
   - add `ChatLens`/`NotebookViewMode` types
   - derive default lens from `viewState.viewMode`
   - add notebook-mode segmented control
   - add DAG/app lens indicator
   - update empty-state and composer copy

2. **Turn context payload**
   - capture `viewMode` and `lens` at submit time
   - pass `context` to `chat_turn`
   - avoid adding lens to session-list/new-session commands

3. **Backend turn framing**
   - define serde payload types
   - prepend deterministic lens preamble to the prompt
   - preserve existing scope/session resolution

4. **Scope parity follow-up**
   - add `chat_resolve_scope` only if frontend/backend key drift becomes a
     source of bugs or duplicated logic grows further

## 11. Test Plan

Frontend tests:

- `defaultLensFor(viewMode, appOpenInfo)` returns the expected lens.
- Notebook mode renders `Builder` and `Deep dive` controls.
- DAG mode renders an operations indicator and no notebook lens toggle.
- App mode renders a product indicator.
- Changing lens updates empty-state/composer copy.
- Submitting a prompt sends `context.viewMode` and `context.lens`.
- Changing lens does not call `chat_new_session`.
- Changing `viewMode` resets a manual lens override.

Backend tests:

- `ChatTurnContext` serde round-trips with camelCase fields.
- `chat_turn` accepts the optional/required context payload as designed.
- prompt framing differs by lens.
- changing lens for the same backend scope keeps the same session id.

Contract tests:

- frontend `chatScopeKey` and backend `AppScope.app_key` stay equivalent for
  notebook and app scopes until `chat_resolve_scope` exists.

## 12. Risks

**Too many visible controls.** The sidebar is narrow. Keep the lens control
compact and only show a two-option toggle in notebook mode.

**Lens confused with navigation.** Lens labels must read as assistant posture,
not a panel switcher. Avoid tab styling.

**Scope drift.** Frontend display scope and backend session scope are still
resolved separately. Keep explicit tests and consider `chat_resolve_scope` as a
follow-up.

**Mode without app context.** App mode can be selected even when no app manifest
is active. App lens copy must not overpromise app tools in that state.

**Prompt overfitting.** Lens preambles should guide the assistant without
forcing a rigid answer format.

## 13. Acceptance Criteria

- The AI sidebar has explicit `viewMode` and `lens` concepts.
- Lens defaults follow the current notebook view mode.
- Notebook mode supports builder and deep-dive lenses.
- DAG mode presents an operations lens.
- App mode presents a product lens.
- Lens applies to the next turn without creating a new session.
- `chat_turn` receives structured turn context.
- Backend scope remains authoritative for session identity.
- The UI remains a single chat panel.
