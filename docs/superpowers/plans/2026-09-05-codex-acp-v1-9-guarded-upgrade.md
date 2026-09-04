# Codex ACP 1.9 Guarded Upgrade Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-09-05-codex-acp-v1-9-guarded-upgrade-design.md`
**Formal @spec cells:** none
**Design epic:** `bd-3v50q` (closed)

**Goal:** Ship exact Codex ACP 1.9.0 with private event storage, no durable auth identity metadata, and an explicit fresh-worker profile activation contract.

**Architecture:** Filter the new auth extension at the lowest SPUR ACP boundary, secure the durable sink at file creation/open, then update exact pins across runtime, CLI, and probes. Keep worker mentions on SPUR-managed delegation and document the unsupported in-place brain boundary.

**Tech Stack:** Rust 2021, Tokio, ACP JSON-RPC, Python `unittest`, Node.js, Z3 rule catalog.

---

### Task 1: Block auth identity extension forwarding

**Task ID:** `auth-extension-guard`

**Files:**

- Modify: `crates/spur-acp/src/connection/native.rs`
- Test: `crates/spur-acp/src/connection/native.rs`

**Depends on:** none

**Acceptance Criteria:**

- [ ] `_auth/status_update` is filtered before `ext_notification_tx`.
- [ ] An unrelated extension method and its JSON params are forwarded unchanged.
- [ ] The focused RED test is observed failing before implementation.
- [ ] `scripts/spur-cargo test -p spur-acp --lib` passes.
- [ ] Post-solve repeats the guarded trace and returns `pass`/`sat`.

**Suggested Worker:** codex

**Scope Boundary:**

- IN scope: the native ACP extension-notification match arm and local helper/tests.
- OUT of scope: `spur-core` session pumps, auth UI, adapter source, existing `.spur/events` files.
- If another file is required, emit `scope_drift` before editing it.

**Solver pre-proof:** Reload `sol_1079dd7ab2cc47e5` (current failure) and `sol_f9e5dc3ae2634c0d` (guarded pass). The invariant excludes `SensitiveEventPersisted` and permits the `RawAuthReceived → RawAuthSanitized → SafeEventForwarded → SafeEventPersisted` trace.

**Implementation:**

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn auth_status_update_is_not_forwarded() {
    let payload = forwardable_ext_notification(
        "_auth/status_update".to_owned(),
        serde_json::json!({"authStatus": {"account": {"email": "private@example.invalid"}}}),
    );
    assert!(payload.is_none());
}

#[test]
fn unrelated_extension_notification_is_forwarded_unchanged() {
    let params = serde_json::json!({"sessionId": "s1", "value": 7});
    let payload = forwardable_ext_notification("_vendor/update".to_owned(), params.clone())
        .expect("unrelated extension should be forwarded");
    assert_eq!(payload.method, "_vendor/update");
    assert_eq!(payload.params, params);
}
```

- [ ] **Step 2: Verify RED**

Run: `scripts/spur-cargo test -p spur-acp --lib auth_status_update_is_not_forwarded`
Expected: FAIL because the filtering helper/behavior is absent.

- [ ] **Step 3: Implement the boundary filter**

```rust
fn forwardable_ext_notification(
    method: String,
    params: serde_json::Value,
) -> Option<ExtNotificationPayload> {
    (method != "_auth/status_update").then_some(ExtNotificationPayload { method, params })
}
```

Use this helper in the existing match arm before sending on `ext_notification_tx`; retain existing Grok usage ingestion before the filter for its methods.

- [ ] **Step 4: Verify GREEN**

Run: `scripts/spur-cargo test -p spur-acp --lib auth_status_update_is_not_forwarded`
Run: `scripts/spur-cargo test -p spur-acp --lib unrelated_extension_notification_is_forwarded_unchanged`
Run: `scripts/spur-cargo test -p spur-acp --lib`

- [ ] **Step 5: SOLVE POST**

Repeat the `workflow.safety_invariant` request from `sol_f9e5dc3ae2634c0d` using the landed filter state. Require `outcome=pass` and `status=sat`; record the new `solve_id` in the completion audit.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "fix(spur-acp): block auth identity extension forwarding"
```

---

### Task 2: Harden durable event-store permissions

**Task ID:** `event-store-permissions`

**Files:**

- Modify: `crates/spur-core/src/event_sink.rs`
- Test: `crates/spur-core/src/event_sink.rs`

**Depends on:** none

**Acceptance Criteria:**

- [ ] On Unix, the events directory becomes `0700`.
- [ ] On Unix, active and pre-existing regular `.ndjson` files become `0600`.
- [ ] Rotation creates a `0600` file.
- [ ] Non-Unix compilation behavior is unchanged through `cfg(unix)`.
- [ ] The focused RED test and `scripts/spur-cargo test -p spur-core --lib event_sink` pass in GREEN.
- [ ] Post-solve returns the unique bit-vector witness `0700`/`0600`.

**Suggested Worker:** codex

**Scope Boundary:**

- IN scope: event-sink file opening, startup hardening, rotation, and Unix-only tests.
- OUT of scope: deleting log contents, modifying the parent `.spur` directory, changing rotation quotas.
- If another file is required, emit `scope_drift` before editing it.

**Solver pre-proof:** Reload `sol_1a1da4307bcb4916` (0755/0644 conflict) and `sol_d433309ef6af4f51` (SAT witness `#b111000000`/`#b110000000`, i.e. `0700`/`0600`).

**Implementation:**

- [ ] **Step 1: Write the failing Unix test**

```rust
#[cfg(unix)]
#[test]
fn opening_sink_hardens_directory_and_existing_event_files() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("events");
    std::fs::create_dir_all(&dir).unwrap();
    let existing = dir.join("existing.ndjson");
    std::fs::write(&existing, b"{}\n").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();

    let state = SinkState::open(&dir, DEFAULT_MAX_BYTES).unwrap();

    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(&state.current_path).unwrap().permissions().mode() & 0o777, 0o600);
}
```

- [ ] **Step 2: Verify RED**

Run: `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-core --lib opening_sink_hardens_directory_and_existing_event_files`
Expected: FAIL with observed permissive mode bits.

- [ ] **Step 3: Implement private opens and startup hardening**

Use `std::os::unix::fs::{OpenOptionsExt, PermissionsExt}` behind `cfg(unix)`. Add one helper that sets the directory to `0o700`, walks only direct regular `.ndjson` entries, and sets them to `0o600`; add one helper used by initial open and rotation that creates/opens the file and enforces `0o600`. Keep non-Unix helpers behaviorally equivalent to the existing open path.

- [ ] **Step 4: Verify GREEN**

Run: `SPUR_REMOTE=0 scripts/spur-cargo test -p spur-core --lib opening_sink_hardens_directory_and_existing_event_files`
Run: `scripts/spur-cargo test -p spur-core --lib event_sink`

- [ ] **Step 5: SOLVE POST**

Repeat the checked bit-vector request from `sol_d433309ef6af4f51` with the landed `0700` and `0600` modes pinned. Require `status=sat` and record the new `solve_id`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/event_sink.rs
git commit -m "fix(spur-core): restrict durable event-store permissions"
```

---

### Task 3: Upgrade the ACP runtime seed to 1.9.0

**Task ID:** `runtime-pin-1-9`

**Files:**

- Modify: `crates/spur-acp/src/config/mod.rs`
- Modify: `crates/spur-acp/src/seed_agents.toml`
- Modify: `crates/spur-acp/src/types.rs`

**Depends on:** `auth-extension-guard`, `event-store-permissions`

**Acceptance Criteria:**

- [ ] The seed command is exactly `@agentclientprotocol/codex-acp@1.9.0`.
- [ ] Install/type documentation names 1.9.0.
- [ ] The version contract test is named for 1.9.0 and was observed RED against the 1.7 seed.
- [ ] `scripts/spur-cargo test -p spur-acp --lib` passes.
- [ ] Version post-solve passes at rank 190.

**Suggested Worker:** codex

**Scope Boundary:**

- IN scope: the three listed runtime/config files.
- OUT of scope: CLI strings, probe scripts, historical specs, ACP behavior changes.
- If another file is required, emit `scope_drift` before editing it.

**Solver pre-proof:** Reload `sol_d0ed070866f345a6` (rank 170 fails) and `sol_7149b18b4b0748a5` (rank 190 passes).

**Implementation:**

- [ ] **Step 1: Change only the test expectation to 1.9.0**

```rust
#[test]
fn seed_template_codex_uses_agentclientprotocol_adapter_1_9_0() {
    let seeds = load_seed_template();
    let codex = seeds
        .entries
        .iter()
        .find(|agent| agent.name == "codex")
        .expect("codex should be in seed template");

    assert_eq!(
        codex.effective_args(),
        vec!["--yes".to_owned(), "@agentclientprotocol/codex-acp@1.9.0".to_owned()]
    );
}
```

- [ ] **Step 2: Verify RED**

Run: `scripts/spur-cargo test -p spur-acp --lib seed_template_codex_uses_agentclientprotocol_adapter_1_9_0`
Expected: FAIL showing actual 1.7.0 and expected 1.9.0.

- [ ] **Step 3: Update the seed and type comment**

Replace active `1.7.0` adapter strings in `seed_agents.toml` and the Codex variant documentation in `types.rs` with exact `1.9.0`. Do not use `latest` or a range.

- [ ] **Step 4: Verify GREEN**

Run: `scripts/spur-cargo test -p spur-acp --lib seed_template_codex_uses_agentclientprotocol_adapter_1_9_0`
Run: `scripts/spur-cargo test -p spur-acp --lib`

- [ ] **Step 5: SOLVE POST**

Repeat `configuration.version_interval` with provider rank 190 and inclusive interval `[190,190]`. Require `outcome=pass`, `status=sat`, and record the new `solve_id`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs crates/spur-acp/src/seed_agents.toml crates/spur-acp/src/types.rs
git commit -m "feat(spur-acp): pin Codex ACP adapter 1.9.0"
```

---

### Task 4: Upgrade CLI installation guidance to 1.9.0

**Task ID:** `cli-pin-1-9`

**Files:**

- Modify: `crates/spur-cli/src/commands/init.rs`
- Test: `crates/spur-cli/tests/init_ux.rs`

**Depends on:** `runtime-pin-1-9`

**Acceptance Criteria:**

- [ ] Global install and npx guidance both use exact 1.9.0.
- [ ] The CLI fixture was observed RED before production strings changed.
- [ ] `scripts/spur-cargo test -p spur-cli --test init_ux` passes.
- [ ] Version post-solve passes at rank 190.

**Suggested Worker:** codex

**Scope Boundary:**

- IN scope: CLI init guidance and its integration test.
- OUT of scope: ACP seed files, unrelated init UX text, docs.
- If another file is required, emit `scope_drift` before editing it.

**Solver pre-proof:** Reload `sol_3b318c1a9a2e45a1` (rank 170 fails) and `sol_cd39652f4ed14a7f` (rank 190 passes).

**Implementation:**

- [ ] **Step 1: Set the test adapter constant to exact 1.9.0**

```rust
const ADAPTER: &str = "@agentclientprotocol/codex-acp@1.9.0";
```

- [ ] **Step 2: Verify RED**

Run: `scripts/spur-cargo test -p spur-cli --test init_ux`
Expected: FAIL because init output still contains 1.7.0.

- [ ] **Step 3: Update both production guidance strings**

Use `npm install -g @agentclientprotocol/codex-acp@1.9.0` and `npx @agentclientprotocol/codex-acp@1.9.0` exactly.

- [ ] **Step 4: Verify GREEN**

Run: `scripts/spur-cargo test -p spur-cli --test init_ux`

- [ ] **Step 5: SOLVE POST**

Repeat the CLI `configuration.version_interval` request at provider rank 190; require pass/SAT and record the `solve_id`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/src/commands/init.rs crates/spur-cli/tests/init_ux.rs
git commit -m "feat(spur-cli): recommend Codex ACP adapter 1.9.0"
```

---

### Task 5: Upgrade and tighten Codex ACP probes

**Task ID:** `probe-pin-1-9`

**Files:**

- Modify: `scripts/probe-codex-acp.mjs`
- Modify: `scripts/probe_acp_subagents.py`
- Test: `scripts/test_probe_acp_subagents.py`

**Depends on:** `runtime-pin-1-9`

**Acceptance Criteria:**

- [ ] Default JavaScript and Python probe commands request exact Codex ACP 1.9.0.
- [ ] Exact evidence identifies bundled `@openai/codex` 0.153.2.
- [ ] A regression test rejects or avoids an inherited mismatched `codex-acp` executable.
- [ ] Historical generic label behavior not tied to the default pin remains supported where tests require it.
- [ ] `python3 -m unittest scripts.test_probe_acp_subagents` passes.
- [ ] Version post-solve passes at rank 190.

**Suggested Worker:** codex

**Scope Boundary:**

- IN scope: the listed probe programs and their unit tests.
- OUT of scope: running a network/live probe, deleting prior artifacts, editing historical RCA/spec files.
- If another file is required, emit `scope_drift` before editing it.

**Solver pre-proof:** Reload `sol_d79f8c08fdd24dea` (rank 170 fails) and `sol_1293250df9b747b9` (rank 190 passes).

**Implementation:**

- [ ] **Step 1: Change test expectations first**

```python
self.assertEqual(probe.DEFAULT_CODEX_PACKAGE, "@agentclientprotocol/codex-acp@1.9.0")
self.assertIn("@agentclientprotocol/codex-acp@1.9.0", command)
self.assertEqual(payload["codexPackageVersion"], "0.153.2")
```

Add a command-resolution regression whose fixture puts a mismatched `codex-acp` on inherited `PATH` and asserts the exact package invocation remains authoritative or the mismatch is reported as failure.

- [ ] **Step 2: Verify RED**

Run: `python3 -m unittest scripts.test_probe_acp_subagents`
Expected: FAIL on the 1.7.0 default and 0.144.1 exact-evidence expectations.

- [ ] **Step 3: Update exact constants and evidence policy**

Set both probe package constants to `@agentclientprotocol/codex-acp@1.9.0`. Set exact 1.9 evidence to Codex `0.153.2`, as declared by the indexed package dependency. Make process resolution verify the executed adapter version instead of accepting an inherited binary merely because its executable name matches.

- [ ] **Step 4: Verify GREEN**

Run: `python3 -m unittest scripts.test_probe_acp_subagents`
Run: `node --check scripts/probe-codex-acp.mjs`
Run: `python3 -m py_compile scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py`

- [ ] **Step 5: SOLVE POST**

Repeat the probe `configuration.version_interval` request at provider rank 190; require pass/SAT and record the `solve_id`.

- [ ] **Step 6: Commit**

```bash
git add scripts/probe-codex-acp.mjs scripts/probe_acp_subagents.py scripts/test_probe_acp_subagents.py
git commit -m "test(acp): probe exact Codex ACP 1.9.0"
```

---

### Task 6: Make profile activation semantics explicit

**Task ID:** `mention-profile-contract`

**Files:**

- Modify: `crates/spur-tui/src/mentions/hint.rs`
- Modify: `docs/spur/agent-onboarding-cookbook.md`
- Test: `crates/spur-tui/src/mentions/hint.rs`

**Depends on:** `runtime-pin-1-9`

**Acceptance Criteria:**

- [ ] Enriched hints map worker→`agent`, agent slot→`profile`, and preserve model/effort.
- [ ] Hints state that profile activation occurs on a fresh SPUR worker session and does not switch the current brain.
- [ ] Bare-worker advisory semantics remain unchanged.
- [ ] Cookbook uses Codex ACP 1.9.0 and explains worker, Codex-native child, and primary-brain boundaries.
- [ ] `scripts/spur-cargo test -p spur-tui mentions::hint` passes.
- [ ] Profile-activation post-solve passes at bound 5; in-place route remains a bounded failure.

**Suggested Worker:** codex

**Scope Boundary:**

- IN scope: mention hint wording/tests and onboarding documentation.
- OUT of scope: automatic tool dispatch, native `spawn_agent`, brain reconnection code, `multi_agent_v2` defaults.
- If another file is required, emit `scope_drift` before editing it.

**Solver pre-proof:** Reload `sol_9869e727050541e5` (in-place target unreachable at bound 2) and `sol_1f87a946bdd7462d` (fresh SPUR worker target reachable at bound 5).

**Implementation:**

- [ ] **Step 1: Add a failing enriched-hint contract test**

```rust
#[test]
fn enriched_worker_hint_explains_profile_activation_boundary() {
    let mut blocks = vec![ContentBlock::Text(TextContent::new("user text"))];
    let ranges = vec![range(
        "worker://codex?agent=reviewer&model=gpt-5.3-codex&effort=high",
    )];
    let known = known(&["codex"]);
    assert!(prepend_worker_hint(&mut blocks, &ranges, &known));
    let hint = hint_text(&blocks).expect("worker hint");

    assert!(hint.contains("worker maps to `agent`"));
    assert!(hint.contains("agent selection maps to `profile`"));
    assert!(hint.contains("fresh SPUR worker session"));
    assert!(hint.contains("does not switch the current brain session"));
}
```

- [ ] **Step 2: Verify RED**

Run: `scripts/spur-cargo test -p spur-tui enriched_worker_hint_explains_profile_activation_boundary`
Expected: FAIL because the current text only describes an advisory preference.

- [ ] **Step 3: Update the enriched hint and cookbook**

Keep the existing override caveat. Add this semantic content: accepted entries use SPUR worker delegation; the worker name maps to `agent`, the mention's agent slot maps to `profile`, model/effort pass through, profile activation occurs on a fresh worker, and the active brain is unchanged. Document that changing the primary brain profile requires a new connection/session because Codex ACP 1.9 has no profile config option.

- [ ] **Step 4: Verify GREEN**

Run: `scripts/spur-cargo test -p spur-tui enriched_worker_hint_explains_profile_activation_boundary`
Run: `scripts/spur-cargo test -p spur-tui mentions::hint`

- [ ] **Step 5: SOLVE POST**

Repeat both workflow requests. Require the SPUR worker trace to remain pass/SAT at bound 5 and the in-place brain trace to remain fail/UNSAT at bound 2. Record both new `solve_id` values.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/mentions/hint.rs docs/spur/agent-onboarding-cookbook.md
git commit -m "docs(spur-tui): clarify Codex profile activation route"
```

---

## DAG

```text
auth-extension-guard ───────┐
                            ├─ runtime-pin-1-9 ─┬─ cli-pin-1-9
event-store-permissions ────┘                   ├─ probe-pin-1-9
                                                └─ mention-profile-contract
```

## Final verification

After all six tasks are approved and integrated:

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-acp --lib
scripts/spur-cargo test -p spur-core --lib event_sink
scripts/spur-cargo test -p spur-cli --test init_ux
scripts/spur-cargo test -p spur-tui mentions::hint
python3 -m unittest scripts.test_probe_acp_subagents
node --check scripts/probe-codex-acp.mjs
rg -n 'codex-acp@1\.7\.0|Codex via .*1\.7\.0' crates/spur-acp crates/spur-cli docs/spur scripts
```

The final `rg` must return no active matches outside explicitly historical records. Review the integrated diff against the source spec, rerun every post-solve, and only then close the implementation epic.
