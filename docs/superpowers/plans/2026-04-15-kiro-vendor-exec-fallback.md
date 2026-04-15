# Kiro vendor-exec fallback — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `/clear` and every other kiro slash command from killing the brain ACP subprocess by routing them through `session/prompt` instead of the broken `_kiro.dev/commands/execute` vendor extension, and hide kiro's client-local commands from the popup.

**Architecture:** Zero code changes in `crates/spur-tui/src/commands/`. The fix is two small edits: (1) flip kiro's `[commands]` dispatch in `seed_agents.toml` from `vendor_exec` to `prompt_text` — the existing `PromptText` dispatch path in `submit_router` already handles this correctly, as proven by a live probe that showed kiro replies "Conversation cleared" and stays alive when `/clear` arrives via `session/prompt`. (2) Teach `run_ingest_hook` to drop incoming commands whose raw JSON carries `meta.local == true`, since those are meant for client-local handling and kiro's `meta` field (unlike ACP's `_meta`) is silently discarded by `AvailableCommand` today.

**Tech Stack:** Rust, TOML, serde_json. Touches `crates/spur-acp/src/seed_agents.toml`, `crates/spur-tui/src/agents/ingest.rs`, `crates/spur-core/tests/init_agents.rs`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-15-kiro-vendor-exec-fallback-design.md`.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-acp/src/seed_agents.toml` | Modify | Flip kiro dispatch; remove now-inert exec/response config. |
| `crates/spur-core/tests/init_agents.rs` | Modify | Update assertions to expect `PromptText` dispatch for kiro. |
| `crates/spur-tui/src/agents/ingest.rs` | Modify | Pre-filter `meta.local=true` JSON items before deserialization. |

---

## Task 1: Flip kiro's dispatch to `prompt_text` and update the init test

**Files:**
- Modify: `crates/spur-acp/src/seed_agents.toml` (the `[agents.entries.commands]` block under the kiro entry — search for `name = "kiro"`)
- Modify: `crates/spur-core/tests/init_agents.rs:114-126` (kiro dispatch assertions)

- [ ] **Step 1: Update the init_agents test to assert the new contract**

Edit `crates/spur-core/tests/init_agents.rs`. Replace the lines 114-126 block (the three assertions about `kiro.commands.dispatch`, `exec_method`, and `response`) with the post-fix contract:

```rust
assert_eq!(kiro.commands.dispatch, spur_acp::DispatchKind::PromptText);
assert!(
    kiro.commands.exec_method.is_none(),
    "prompt_text dispatch should not carry an exec_method"
);
assert!(
    !kiro.commands.ingest.is_empty(),
    "kiro should still ingest commands/available notifications"
);
assert!(
    kiro.commands.response.is_empty(),
    "prompt_text dispatch has no vendor-exec response to render"
);
```

Keep the existing `effective_permissions` assertions (lines 127-134) unchanged — they're unrelated.

- [ ] **Step 2: Run the test to verify it fails against the current seed**

Run: `cargo test -p spur-core --test init_agents -- --nocapture`
Expected: FAIL with an assertion about `dispatch` being `VendorExec` instead of `PromptText`.

- [ ] **Step 3: Flip the kiro seed config**

Edit `crates/spur-acp/src/seed_agents.toml`. Find the kiro entry (starts at `name = "kiro"`). Replace the `[agents.entries.commands]` block, the `[[agents.entries.commands.ingest]]` block, and the `[[agents.entries.commands.response]]` block so that only the ingest binding remains and dispatch is `prompt_text`:

```toml
[agents.entries.commands]
dispatch = "prompt_text"

[[agents.entries.commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "commands"
item_schema = "acp_available_command"
```

Do NOT touch `[agents.entries]`, `[agents.entries.display]`, or `[agents.entries.permissions]`.

- [ ] **Step 4: Re-run the init_agents test**

Run: `cargo test -p spur-core --test init_agents -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full spur-acp + spur-core test suites to catch anything that relied on the old config**

Run: `cargo test -p spur-acp -p spur-core`
Expected: PASS.

If a test in `crates/spur-acp/tests/nested_config_shape.rs` fails, inspect it — that file uses its OWN inline TOML (not the seed), so it should keep working. If it doesn't, re-read it before making changes.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/seed_agents.toml crates/spur-core/tests/init_agents.rs
git commit -m "fix(seed): route kiro slash commands through prompt_text

The _kiro.dev/commands/execute vendor extension in kiro-cli 2.0.0 kills
the ACP subprocess for every command tested (/clear, /compact, /help,
and eight others). Sending the same commands via session/prompt works
correctly — kiro handles them inline and replies normally.

Flip the dispatch kind in the seed; ingest remains so the popup still
lists kiro's commands."
```

---

## Task 2: Drop `meta.local=true` items at ingest

**Files:**
- Modify: `crates/spur-tui/src/agents/ingest.rs`

- [ ] **Step 1: Write the failing test**

Append this test to the `tests` mod at the bottom of `crates/spur-tui/src/agents/ingest.rs` (before the final closing `}` of `mod tests`):

```rust
#[test]
fn filters_entries_marked_meta_local_true() {
    let binding = IngestBinding {
        method: "_kiro.dev/commands/available".into(),
        parser: IngestParserKind::JsonPathList,
        path: "commands".into(),
        item_schema: ItemSchemaKind::AcpAvailableCommand,
    };
    let params = serde_json::json!({
        "commands": [
            { "name": "/agent", "description": "switch agent", "meta": { "local": false } },
            { "name": "/chat",  "description": "save/load",    "meta": { "local": true } },
            { "name": "/compact", "description": "summarize" }
        ]
    });
    let out = run_ingest_hook(&binding, &params).expect("decoded");
    let names: Vec<_> = out.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["/agent", "/compact"], "meta.local=true entries must be dropped");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --lib agents::ingest::tests::filters_entries_marked_meta_local_true`
Expected: FAIL — without the filter, all three entries survive and the `assert_eq!` fails showing `["/agent", "/chat", "/compact"]`.

- [ ] **Step 3: Add the pre-filter to `run_ingest_hook`**

Replace the body of `run_ingest_hook` in `crates/spur-tui/src/agents/ingest.rs` (lines 12-23) with this version. The change is: after resolving the list, filter out items carrying `meta.local == true` at the raw JSON level (ACP's `AvailableCommand` renames the field to `_meta`, so kiro's `meta` would be silently dropped by `from_value` — we must look before that happens).

```rust
pub fn run_ingest_hook(binding: &IngestBinding, params: &Value) -> Option<Vec<AvailableCommand>> {
    match binding.parser {
        IngestParserKind::JsonPathList => {
            let list = lookup_dotted_path(params, &binding.path)?;
            let Value::Array(items) = list else { return None };
            let filtered: Vec<Value> = items
                .into_iter()
                .filter(|item| {
                    item.get("meta")
                        .and_then(|m| m.get("local"))
                        .and_then(|v| v.as_bool())
                        != Some(true)
                })
                .collect();
            match binding.item_schema {
                ItemSchemaKind::AcpAvailableCommand => {
                    serde_json::from_value::<Vec<AvailableCommand>>(Value::Array(filtered)).ok()
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run the new test and the existing ingest tests**

Run: `cargo test -p spur-tui --lib agents::ingest`
Expected: all four tests PASS (three pre-existing + the new `filters_entries_marked_meta_local_true`).

- [ ] **Step 5: Run the full spur-tui test suite to make sure no integration test relied on `local:true` entries surviving**

Run: `cargo test -p spur-tui`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/agents/ingest.rs
git commit -m "feat(ingest): drop agent-advertised commands with meta.local=true

ACP's AvailableCommand renames its metadata field to \"_meta\", so kiro's
\"meta\" payload is silently discarded by serde. Pre-filter at the raw
JSON level: any item carrying meta.local=true (client-handled, e.g.
/chat save, /chat load) is dropped before deserialization so it never
reaches the command popup."
```

---

## Task 3: Manual acceptance probe

**Files:** none — this is a live check that validates the spec's acceptance criteria end-to-end.

- [ ] **Step 1: Build the TUI with the fixed config**

Run: `cargo build -p spur-tui`
Expected: build succeeds.

- [ ] **Step 2: Launch spur against kiro and confirm `/clear` works**

Run: `cargo run -p spur-cli -- --brain kiro` (or whatever launch command this repo uses — consult `README.md` if unsure).

In the TUI:
1. Send any normal prompt (e.g. "hi") and wait for a reply.
2. Type `/clear` and press Enter.

Expected:
- No `BRAIN ERROR` banner.
- Kiro streams "Conversation cleared" (or equivalent) as agent text.
- The session remains usable — a follow-up prompt round-trips.

- [ ] **Step 3: Confirm `/chat save` / `/chat load` no longer appear in the popup**

In the TUI, open the slash-command popup (type `/`). Scroll through the kiro-provided entries.

Expected:
- `/agent`, `/clear`, `/compact`, `/help`, `/context`, etc. are present.
- `/chat save` and `/chat load` (any entry kiro marks `local:true`) are **absent**.

- [ ] **Step 4: Confirm the wire protocol matches the spec acceptance criterion**

With the TUI running, tail the spur log:
`tail -f .spur/logs/spur.log.$(date +%Y-%m-%d)`

Trigger `/clear` in the TUI. In the log, verify:
- An outgoing `session/prompt` RPC containing text `/clear` (look for `send:` lines or `NativeAcpConnection: sending prompt`).
- No `_kiro.dev/commands/execute` RPC anywhere.

If either expectation fails, return to Task 1 / Task 2 and debug before marking the plan done.

- [ ] **Step 5: No commit for this task** — it's a verification-only step. Note the result in the PR description when the plan is wrapped up.

---

## Self-Review

- **Spec coverage:**
  - A1 (dispatch flip) → Task 1.
  - A2 (meta.local filter) → Task 2.
  - Acceptance criteria 1, 2, 5 (manual kiro run, wire-protocol absence of commands/execute) → Task 3.
  - Acceptance criterion 3 (chat entries absent) → Task 3 step 3.
  - Acceptance criterion 4 (`cargo test` passes) → Task 1 step 5 + Task 2 step 5.
- **Placeholder scan:** No TBDs, no "handle edge cases", no deferred implementations. Every code block is complete.
- **Type consistency:** `IngestBinding`, `IngestParserKind`, `ItemSchemaKind`, `AvailableCommand`, `Value`, `DispatchKind::PromptText` all match their existing definitions in `spur_acp` and `serde_json`. The test helper uses the same `IngestBinding` struct-literal shape used in `ingest.rs`'s existing tests.
