# spur-acp maintenance guide

spur-acp is SPUR's ACP (Agent Client Protocol) client layer. Its job: speak the
**stable ACP spec** on one side, and absorb **real agents' wire deviations** on
the other — without leaking vendor quirks into spur-tui / spur-core.

Two rules govern all changes here:

1. **Spec-stable behavior belongs in `src/connection/` (native.rs) and the
   schema-driven types.** If the ACP spec fully defines it, do not special-case
   it per agent.
2. **Anything an agent does that is NOT covered by the stable ACP spec —
   vendor `_meta` tunnels, non-standard payloads, missing spec fields,
   re-advertisement conventions — is handled by a per-agent standardizer in
   `src/adapter/`, discovered through probing.**

---

## Probe first, standardize second

Never guess an agent's wire behavior. Discover it with the probe harness:

```bash
# handshake-only (free)
python3 scripts/probe_acp_capabilities.py --command <agent-cmd> \
  --args "<argv>" --label <name> --always-approve --quiet

# full battery: config-option sets, vendor RPCs, one billed prompt
python3 scripts/probe_acp_capabilities.py ... --probe-vendor-rpc \
  --try-set-model --prompt "<tiny shell task>"
```

- Output: `.spur/logs/probe-<label>-*.jsonl` (raw frames) + `.report.json`
  (config options, set results, update variants, vendor methods).
- Unit tests for the probe itself: `python3 -m unittest
  scripts.test_probe_acp_capabilities` (extend when adding probe surfaces).
- For frame-level questions (streaming shapes, `_meta` payloads, error
  bodies), replay the captured jsonl — do not re-bill the agent.
- Existing probe evidence: grok-acp (terminal side-channel, per-model effort
  narrowing, `config_option_update` re-advertisement), codex-acp 1.12.0
  (internal exec, `rawOutput` tunneling, static effort list), pi-acp
  (`_meta.terminal_output` streaming). Specs live in
  `docs/superpowers/specs/2026-09-01-dynamic-acp-capability-evidence-design*.ipynb`.

## The adapter layer (`src/adapter/`)

When the probe shows an agent deviating from the stable spec, fix it **here**,
not in the connection layer:

| File | Agent | Quirk handled |
|------|-------|---------------|
| `kimi.rs` | Kimi | raw_output synthesis from known tool inputs |
| `gemini.rs` | Gemini | tool_call kind normalization |
| `codex.rs` | Codex | tool-call meta tunneling |
| `pi.rs` | Pi | `_meta.terminal_output` chunks → cumulative `raw_output`; `_meta.terminal_exit` → Completed/Failed |
| `grok_session_display.rs`, `kiro_session_display.rs` | Grok / Kiro | session display models |

Contract for a standardizer (`SessionEventStandardizer` in `mod.rs`):

- One module per agent, selected via `for_agent(AgentKind)`.
- Stateful accumulators must be bounded (see `pi.rs::ACCUMULATOR_CAP_BYTES`).
- Standardized output must be **spec-shaped**: downstream consumers only ever
  see spec `SessionUpdate` variants with spec fields. Never pass vendor
  `_meta` through as a data channel.
- Updating `raw_output` on streaming updates must be **cumulative** — the TUI
  replaces act text per update (`react_trace` semantics).
- Every standardizer needs unit tests built from **real captured frames**
  (paste from the probe jsonl, like `pi.rs::tests`).

Adding a new agent kind: extend `AgentKind` (`src/types.rs`), add the seed
entry (`src/seed_agents.toml`), write the standardizer if (and only if) the
probe shows deviations, and register it in `SessionEventStandardizer`.

## Capability freshness

Some agents re-advertise `configOptions` out-of-band (`config_option_update`)
or per model. `SpurAgentCaps` (`src/spur_agent_caps.rs`) owns that state;
`apply_config_option_snapshot` refreshes frozen caps and **must ignore empty
snapshots**. Set-responses carrying fresh `configOptions` are refreshed on the
`session/set_config_option` / set-model paths — do not remove either refresh
path; agents differ in which one they use (grok/pi re-advertise, codex only
echoes in the set-response).

## Testing & hygiene

- Local (remote VM flaky): `SPUR_REMOTE=0 SPUR_SCCACHE_S3=0 scripts/spur-cargo
  test -p spur-acp --lib`; remote is the default otherwise.
- Adapter/round-trip regressions: extend `tests/` (see
  `executor_events_roundtrip.rs` pattern).
- `cargo fmt` always local; clippy must be warning-free for touched files.
- Bug fixes: failing `test(...)` commit first, then `fix(...)`. Document newly
  discovered wire quirks in the module doc comment with the agent + version
  observed and the probe date (see `pi.rs` header).
