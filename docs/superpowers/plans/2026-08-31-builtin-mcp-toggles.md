# Built-in MCP Toggles Implementation Plan

> **For SPUR orchestrator:** beads-tracked DAG; brain-side direct delegation (established path).

**Source spec:** `docs/superpowers/specs/2026-08-31-builtin-mcp-toggles-design.ipynb` (gate MCP-NOTEBOOK-INJECTION verified 6/6 fresh)
**Design epic:** `bd-ixbyj`
**Decisions:** all-builtins toggleable (user override, on record), confirm dialog `sol_a660279275f74ed7`, per-tool staged out `sol_c013dfee25984d42`.

**Goal:** Display + probe + runtime toggles for `spur-mcp` / `notebook` / `spur-worker-mcp` in `/configure mcp`, applied at next session start.

**Worker routing:** codex / rust-engineer / gpt-5.6-sol / xhigh.

---

### Task 1 (builtin-t1): spur-acp overrides schema + patch

**Files:** Modify `crates/spur-acp/src/config/mod.rs` only (inline tests).

**Depends on:** none.

**Acceptance:**
- [ ] `BuiltinMcpServer` enum {SpurMcp, Notebook, SpurWorkerMcp} (snake_case serde)
- [ ] `BuiltinMcpOverridesConfig { spur_mcp_enabled, notebook_enabled, worker_mcp_enabled }` default **all true** (manual Default impl — serde `default` on struct gives false), nested `McpServersConfig.builtin_overrides`, skip_serializing when all-true
- [ ] `ConfigPatch::BuiltinMcpToggle { server, enabled }`, `section_id() == "mcp"`, `apply()` writes the matching field
- [ ] Tests: defaults-all-true; TOML round-trip with one false; skip-when-all-true (empty config serialize unchanged); toggle apply per server; existing mcp_servers_config tests untouched and green
- [ ] `scripts/spur-cargo test -p spur-acp` green; no new clippy warnings

**TDD sketch (RED first):**

```rust
#[test]
fn builtin_overrides_default_all_true() {
    let cfg = SpurConfig::default();
    let o = &cfg.mcp_servers.builtin_overrides;
    assert!(o.spur_mcp_enabled && o.notebook_enabled && o.worker_mcp_enabled);
}

#[test]
fn builtin_toggle_persists_and_applies() {
    let mut cfg = SpurConfig::default();
    ConfigPatch::BuiltinMcpToggle { server: BuiltinMcpServer::Notebook, enabled: false }
        .apply(&mut cfg).unwrap();
    assert!(!cfg.mcp_servers.builtin_overrides.notebook_enabled);
    assert!(cfg.mcp_servers.builtin_overrides.spur_mcp_enabled);
    // section id
    assert_eq!(ConfigPatch::BuiltinMcpToggle { server: BuiltinMcpServer::SpurMcp, enabled: true }.section_id(), "mcp");
}

#[test]
fn all_true_overrides_serialize_to_nothing() {
    let cfg = SpurConfig::default();
    let s = toml::to_string(&cfg).unwrap();
    assert!(!s.contains("builtin_overrides"));
}
```

Commit: `test(spur-acp): builtin-t1 builtin override contract` then `feat(spur-acp): builtin-t1 overrides schema and ConfigPatch toggle`.

**Scope drift:** anything beyond `config/mod.rs` → signal.

---

### Task 2 (builtin-t2): spur-core injection gating

**Files:** Modify `crates/spur-core/src/notebook.rs`, `crates/spur-core/src/orchestrator/session.rs` (2 sites), `crates/spur-core/src/orchestrator/adhoc.rs`, `crates/spur-core/src/orchestrator/worker_mcp.rs` (default threading only); extend `crates/spur-core/tests/notebook_mcp_config.rs`.

**Depends on:** builtin-t1.

**Acceptance:**
- [ ] `brain_mcp_servers` honors `spur_mcp_enabled` (skip entry) and notebook meet-semantics (compile feature OR runtime flag → skip; gate MCP-NOTEBOOK-INJECTION)
- [ ] Worker dispatch `enable_worker_mcp` default = `worker_mcp_enabled` (explicit per-delegation flag still wins; `build_worker_mcp_servers_with` helper unchanged)
- [ ] Tests: spur-mcp skip → notebook-only list; notebook runtime-off → `[spur-mcp]`; both notebook flags off → same; worker default threading unit test; baseline default = today's 2 entries
- [ ] `scripts/spur-cargo test -p spur-core` green; no new clippy warnings

Commit: `test(spur-core): builtin-t2 builtin injection gating` then `feat(spur-core): builtin-t2 honor builtin overrides`.

**Scope drift:** `worker_server.rs`, ACP envelope, or non-listed orchestrator files → signal.

---

### Task 3 (builtin-t3): TUI built-in block, toggle + confirm dialog, probe entries

**Files:** Modify `crates/spur-tui/src/views/mcp_servers_tui.rs`; add `crates/spur-tui/src/views/builtin_confirm.rs` (dialog pane); register in `crates/spur-tui/src/views/mod.rs`.

**Depends on:** builtin-t1 (uses `BuiltinMcpServer`, `ConfigPatch::BuiltinMcpToggle`).

**Acceptance:**
- [ ] "Built-in servers" block: 3 rows (name, kind, runtime source, state badge; disabled rows dimmed)
- [ ] `d` toggles any → SAVE-APPLY `ConfigPatch::BuiltinMcpToggle`; disabling **spur-mcp** routes through the confirm dialog (cancel = no patch); re-enabling needs no dialog
- [ ] `t` probes built-ins via transient synthesized `McpServerEntry`s (notebook binary+nonce via existing resolver path surfaced through the pane's hook; spur-mcp runtime url from app state; worker: url+token when live, else "not running" hint)
- [ ] Tests: block renders with badges; toggle emits correct patch per server; spur-mcp disable opens dialog, confirm emits patch + cancel emits none; notebook/worker toggle skip dialog; probe hook invoked with transient entry
- [ ] `scripts/spur-cargo test -p spur-tui` green; no new clippy warnings

Commit: `test(spur-tui): builtin-t3 builtin block and confirm gate` then `feat(spur-tui): builtin-t3 builtin toggles with confirm dialog`.

**Scope drift:** `agent_config_browser.rs` / `nav.rs` / `session_config.rs` (no new ConfigPatch exhaustiveness — BuiltinMcpToggle rides the existing Mcp arms only if the match uses `..`-free per-variant arms: **check first**; if `apply_config_live_hooks`'s MCP arm needs a variant addition, that single arm is pre-authorized in `session_config.rs`) → anything else, signal.

---

## Self-Review

1. **Spec coverage:** schema/T1, injection+worker-default/T2, pane+dialog+probe/T3 — matches spec Components 1–3; guardrail decision implemented as dialog. ✔
2. **Placeholders:** none; dialog + transient-entry synthesis are described with anchors. ✔
3. **Type consistency:** `BuiltinMcpServer`/`BuiltinMcpOverridesConfig`/`ConfigPatch::BuiltinMcpToggle` identical across tasks. ✔
4. **DAG:** T1 → {T2, T3}. ✔
5. **beads:** one issue per task, acceptance criteria reviewable, scope boundaries + pre-authorized exceptions stated. ✔
