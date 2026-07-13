# Grok read-only model / effort status — design

**Status:** shipped baseline; interactive write follow-up implemented after the
0.2.99 re-probe proved `session/set_model` (see the probe results §4.1 and
`docs/superpowers/plans/2026-07-13-grok-interactive-model-effort.md`)
**Date:** 2026-07-13
**Owner:** Kevin Truong
**Predecessors:**
- `2026-07-13-grok-acp-capability-probe-results.md` — live Grok 0.2.93 matrix; empty `configOptions`; proprietary model/effort planes
- `2026-04-28-agent-model-effort-surface-design.md` — status-bar model/effort segments consume `SpurAgentCaps` labels
- `2026-04-27-acp-capability-aware-spur-design.md` — frozen-per-session `SpurAgentCaps`

**Related shipped fixes (orthogonal):**
- `normalize_grok_terminal_command` — packed `terminal/create` argv interop
- `list_sessions_from_disk` empty for declared kinds without a disk layout

---

## 1. Goal

When Grok is hosted under SPUR, the **status bar shows the active model and effort** after `session/new` / `session/load`, using data Grok already returns on the wire but not via standard ACP `configOptions`.

After this work:

- A Grok brain session populates the existing status-bar model and effort segments
  (same components Codex uses; no new chrome).
- Codex / Claude / other agents are **unchanged**.
- SPUR still does **not** advertise interactive `/model` or `/effort` for Grok (no fake switch path).

## 2. Non-goals

| Non-goal | Why |
|---|---|
| Interactive mid-session `/model` or `/effort` | `session/set_config_option` is Method not found; synthesizing pickers would lie |
| Calling `session/set_model` | Out of scope; real model ids unproven for set; separate probe follow-up |
| Teaching the synthesizer about `_meta` for all agents | Vendor-neutral synthesizer stays configOptions-only |
| Persisting model history or cost attribution changes | Status is live freeze at session create/load only |
| Session list / resume over proprietary Grok storage | Separate problem; disk discovery correctly returns `[]` |
| Upstream Grok protocol fix | Preferred long-term, not this delivery |

When Grok later advertises real `configOptions` + `set_config_option`, this adapter becomes a **no-op fallback** (standard plane wins) and can eventually be deleted.

## 3. Problem (verified)

### 3.1 SPUR trust plane

```text
configOptions (select model / thought_level)
        │
        ▼
SpurAgentCaps::current_model_label / current_effort_label
        │
        ▼
SessionDetailView::resolved_*_label → status bar
```

Slash `/model` / `/effort` are synthesized only from the same configOptions plane (`adapter/config_options.rs`). Empty options ⇒ no slash, no labels.

### 3.2 Grok 0.2.93 wire (probe)

| Source | Present? | Survives ACP SDK deserialize? |
|---|---|---|
| `session/new.configOptions` | empty | yes (empty) |
| Top-level `session/new.models` | yes (`currentModelId`, `availableModels`, `reasoningEfforts`) | **no** — not on `NewSessionResponse` schema 1.1; dropped |
| `initialize._meta.modelState` | yes | **yes** (`Meta = Map<String, Value>`) |
| `session/new._meta["x.ai/sessionConfig"]` | yes (categories `model` + `mode` for effort) | **yes** |
| `session/set_config_option` | Method not found | n/a |

So the data SPUR needs for **display** is already on responses that reach `SpurAgentCaps::new` — inside `_meta` — without any new transport capture.

### 3.3 User-felt bug

Grok sessions look model-less under SPUR even though the agent is running `grok-4.5` / high effort. Users cannot tell what they paid for without leaving SPUR.

## 4. Approaches considered

### A — Synthetic configOptions (rejected for v1)

At freeze time, invent `SessionConfigOption` select rows from Grok meta and stuff them into `caps.config_options`.

- **Pros:** Reuses existing label + slash machinery.
- **Cons:** `supports_set_config_option` becomes true; synthesizer emits `/model` / `/effort` that call a missing method (−32601). Fixing that needs a second “read-only option” concept on the standard path — too invasive for display-only.

### B — Separate display labels on `SpurAgentCaps` (recommended)

Extract optional `display_model_label` / `display_effort_label` (or a small nested struct) at freeze time for Grok only. Label accessors fall back to these **after** configOptions resolution fails.

- **Pros:** Cannot create fake slash commands; set_* gates stay false; clean removal when upstream ships configOptions.
- **Cons:** Small Grok-specific parse module; effort category is `"mode"` not `thought_level` (documented quirk).

### C — Raw JSON capture of top-level `models` (deferred)

Keep a side channel of the untyped `session/new` result to recover the dropped `models` field.

- **Pros:** Matches Grok’s richest catalog shape.
- **Cons:** Extra plumbing in native connection; `_meta` already carries selected model + effort labels for status. Revisit only if meta proves incomplete on a future Grok build.

**Recommendation: B.** Prefer `_meta` sources that already deserialize. Prefer display-only fields that never enter the synthesizer.

## 5. Design

### 5.1 New module (spur-acp)

`crates/spur-acp/src/adapter/grok_session_display.rs` (name bikeshed OK):

```rust
/// Labels derived from Grok proprietary meta — never used for set_* or slash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrokSessionDisplay {
    pub model_label: Option<String>,
    pub effort_label: Option<String>,
    /// Raw ids when useful for telemetry / debug; not required for TUI.
    pub model_id: Option<String>,
    pub effort_id: Option<String>,
}

/// Extract display labels from initialize + session/new|load meta.
/// Returns `None` when agent_kind != Grok or no usable keys found.
pub fn extract_grok_session_display(
    agent_kind: AgentKind,
    initialize_meta: Option<&Meta>,
    session_meta: Option<&Meta>,
) -> Option<GrokSessionDisplay>;
```

**Extraction priority (first hit wins per field):**

| Field | Prefer | Then |
|---|---|---|
| model | `session_meta["x.ai/sessionConfig"].options` where `category == "model"` and `selected == true` → `label` (fallback `id`) | `initialize_meta["modelState"].currentModelId` resolved against `availableModels[*].name` if present, else raw id |
| effort | same sessionConfig where `category == "mode"` and `selected == true` → `label` (fallback `id`) | `modelState.availableModels[current]._meta.reasoningEffort` resolved against `reasoningEfforts[*].label` if present, else raw value |

Rationale for treating sessionConfig `"mode"` as effort: live probe maps high/medium/low effort under `category: "mode"`. Document this as a Grok 0.2.93 quirk; if Grok renames category later, probe + unit fixtures update.

**Hard rules:**

1. Call only when `agent_kind == AgentKind::Grok`.
2. Never write into `config_options`.
3. Never invent choices lists for pickers.
4. Malformed / missing meta ⇒ `None` fields (status segment hidden), not an error.

### 5.2 `SpurAgentCaps` integration

```rust
pub struct SpurAgentCaps {
    pub agent: AgentCapabilities,
    pub modes: Option<SessionModeState>,
    pub config_options: Vec<SessionConfigOption>,
    pub agent_kind: AgentKind,
    /// Grok-only; always None for other kinds.
    pub grok_display: Option<GrokSessionDisplay>,
}

impl SpurAgentCaps {
    pub fn new(initialize, new_session, agent_kind) -> Self {
        let config_options = new_session.config_options.clone().unwrap_or_default();
        let grok_display = extract_grok_session_display(
            agent_kind,
            initialize.meta.as_ref(),
            new_session.meta.as_ref(),
        );
        Self { /* … */, config_options, grok_display }
    }

    pub fn current_model_label(&self) -> Option<String> {
        Self::model_label_from_config_options(&self.config_options)
            .map(str::to_owned)
            .or_else(|| {
                self.grok_display
                    .as_ref()
                    .and_then(|d| d.model_label.clone())
            })
    }

    pub fn current_effort_label(&self) -> Option<String> {
        Self::effort_label_from(&self.config_options).or_else(|| {
            self.grok_display
                .as_ref()
                .and_then(|d| d.effort_label.clone())
        })
    }
}
```

Same for `from_loaded` using `LoadSessionResponse.meta` (if present; empty meta ⇒ labels None).

**Invariant:** `supports_set_model` / `supports_set_config_option` remain configOptions-only. Grok with empty options still reports both false. `synthesize` / `synthesize_advertised` untouched.

### 5.3 TUI consumption

No TUI-specific Grok branch required if:

- `SessionDetailView::resolved_model_label` already falls back to `caps.current_model_label()` after live `session_config_options`.
- Same for effort.

Verify (and add a regression test if missing) that when `session_config_options` is empty and caps carry `grok_display`, status bar props receive non-empty labels.

**Do not** call `AdvertisedSource::entries_from_caps` in a way that would invent model/effort commands from `grok_display`. Today that path only uses configOptions synthesis — leave it.

### 5.4 Lifecycle

| Event | Behavior |
|---|---|
| `session/new` | Freeze labels from meta |
| `session/load` | Freeze from load response meta (same extractor) |
| Mid-session Grok model change outside SPUR | **Not observed** (no config_option_update / set path). Labels stay at freeze until re-create/load. Acceptable for v1; document. |
| Grok ships real configOptions | Standard labels win; `grok_display` is dead data; optional later cleanup |

### 5.5 Removal criteria

Delete `grok_display` + extractor when a live `scripts/probe_grok_acp.py` report shows:

- `config_options_advertised: true`
- `model_select_advertised: true`
- preferably `session/set_config_option` accepted for a real id

Gate documented in the probe results changelog.

## 6. Testing

| Layer | Cases |
|---|---|
| Unit: `extract_grok_session_display` | Fixture JSON from probe (`sessionConfig` selected model + mode; `modelState` only; missing meta; non-Grok kind → None; partial selected flags) |
| Unit: `SpurAgentCaps` | Grok empty configOptions + meta → labels Some; Grok with real configOptions → configOptions win; Codex unchanged |
| Unit: synthesizer | Grok caps with grok_display still emit **zero** `/model` `/effort` commands |
| TUI (if cheap) | `resolved_model_label` / `resolved_effort_label` with empty session options + grok caps |
| Manual | Start Grok brain under SPUR; confirm status bar model/effort after handshake |

Fixtures: store minimal maps under `crates/spur-acp/tests/fixtures/grok/` derived from `.spur/logs/probe-grok-20260712T223617.report.json` (trim secrets; meta only).

## 7. Files likely touched

| Path | Change |
|---|---|
| `crates/spur-acp/src/adapter/grok_session_display.rs` | **new** extractor |
| `crates/spur-acp/src/adapter/mod.rs` | mod + re-export if needed |
| `crates/spur-acp/src/spur_agent_caps.rs` | field + accessor fallback + construction |
| `crates/spur-acp/tests/…` or inline unit tests | fixtures |
| `docs/superpowers/specs/2026-07-13-grok-acp-capability-probe-results.md` | §5 note: SPUR read-only status adapter planned/shipped |
| `crates/spur-acp/src/seed_agents.toml` / cookbook | one-line: status may show model from meta; still no mid-session switch |

No `spur-tui` change expected unless a regression test fails (then wire-only).

## 8. Risks

| Risk | Mitigation |
|---|---|
| Grok renames `_meta` keys | Extractor returns None; probe is regression gate |
| Treating `"mode"` as effort wrong for other Grok modes later | Only map known effort ids (`high`/`medium`/`low`) or require selected option labels matching reasoning efforts; if ambiguous, show raw label only when id is in the known set |
| Users think status implies switchable model | UX copy stays passive segments (existing status bar); seed comments already warn no `/model` |
| Double labels if both configOptions and grok_display | configOptions always first |

**Ambiguity call (locked):** effort extraction from sessionConfig only accepts selected options whose `id` is in `{high, medium, low}` **or** whose label matches probe reasoning effort labels. Unknown `"mode"` categories (if Grok later adds real modes) are ignored for effort_label.

## 9. Success criteria

1. With Grok 0.2.93 (or successor with same meta shape), SPUR status bar shows non-empty model label after session create.
2. Effort label shows when high/medium/low is selected in sessionConfig (or modelState reasoningEffort).
3. `supports_set_model` / synthesized `/model` remain false for empty configOptions.
4. Non-Grok agents: zero behavioral change (unit-guarded).
5. Probe doc updated with “SPUR read-only status” row once shipped.

## 10. Out-of-band follow-ups (not this design)

1. Probe `session/set_model` with live ids (`grok-4.5`, `grok-composer-2.5-fast`).
2. Optional write path if set_model works.
3. Upstream request: advertise standard `configOptions` + implement `set_config_option`.
4. Cookbook model id example: `grok-build` → `grok-4.5` (docs nit).

## 11. Open questions for review

None blocking implementation. Optional product preference:

- Prefer display name (`Grok 4.5`) vs raw id (`grok-4.5`) when both exist → **prefer display name** (matches Codex status behavior).

---

## Appendix A — Example meta shapes (from probe)

```json
// session/new._meta["x.ai/sessionConfig"]
{
  "options": [
    { "id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": true },
    { "id": "high", "category": "mode", "label": "High Effort", "selected": true }
  ]
}
```

```json
// initialize._meta.modelState (abbrev)
{
  "currentModelId": "grok-4.5",
  "availableModels": [
    {
      "modelId": "grok-4.5",
      "name": "Grok 4.5",
      "_meta": {
        "reasoningEffort": "high",
        "reasoningEfforts": [
          { "id": "high", "label": "High Effort", "default": true }
        ]
      }
    }
  ]
}
```
