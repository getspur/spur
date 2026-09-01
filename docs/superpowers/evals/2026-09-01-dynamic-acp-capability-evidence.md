# Dynamic ACP capability evidence — post-implementation evaluation

- **Task:** `bd-2f4b`
- **GREEN base:** `d78b16aa8de4674007ebd5a701fc21794af14bc6`
- **RED predecessor:** `4ee2cdd3d46ee2d76b97ce30c4b77b4e4d6cf8ea`
- **Production dependency:** `bd-2a60` (closed/approved)
**POST Optimize solve:** `sol_b61231622c374fc0` (`sat`, complete)

The POST model selected `bind_accepted_probe_to_semantic_capability=1`,
`promote_vendor_advertisement_to_native=0`, and
`bypass_reducer_with_legacy_router_override=0`. The fixture replays and live
post-probes agree with that model: advertisement alone is `PromptOnly`, while a
paired successful `session/set_model` response binds `Model/model` as
`NativePreferred`. No reducer/runtime mismatch or scope-drift signal was found.

## Probe safety and sanitization

Both live probes used only `initialize`, `session/new`, passive notifications,
and direct handshake/vendor RPCs. Neither command supplied `--prompt`, so no
billed model turn was sent. The temporary reports and JSONL logs were kept
outside the repository and were removed after fixture validation.

Before publication, every session identifier was replaced with a deterministic
`<session-N>` placeholder and remaining opaque UUIDs with `<uuid-N>`;
home/worktree paths were replaced with `<home>`,
`<workspace>`, or `<probe-wrapper>`; credential-bearing values were replaced
with `<redacted>`; and every affected raw-payload, claim-reference, and fixture
digest was recomputed. A recursive post-sanitization scan found zero raw home
paths, zero raw worktree paths, zero unsanitized session fields, and zero
unredacted values under credential-bearing keys in both fixtures.

## Exact live post-probes

Version checks:

```text
/tmp/spur-acp-grok-wrapper.J9b5qf/grok-probe --version
grok 1.0.13 (5e9a58528b76) [stable]

/usr/bin/env kiro-cli --version
kiro-cli 2.20.2
```

The temporary Grok wrapper contained only
`exec /usr/bin/env grok "$@"`; it let the probe obtain Grok's actual version
while keeping the resolved executable identity free of a home-directory path.

Grok, observed at `2026-09-01T21:47:37.598Z`:

```bash
python3 scripts/probe_acp_capabilities.py \
  --command /tmp/spur-acp-grok-wrapper.J9b5qf/grok-probe \
  --args "agent stdio" \
  --label grok-1.0.13-post \
  --cwd /tmp \
  --out /tmp/spur-acp-post.J9b5qf/grok2.jsonl \
  --report /tmp/spur-acp-post.J9b5qf/grok2.report.json \
  --try-set-model \
  --probe-vendor-rpc \
  --always-approve \
  --init-timeout 30 \
  --session-timeout 45 \
  --preamble-timeout 1 \
  --quiet
```

Result: exit `0`; dynamic models were `grok-4.5` and `grok-4.6`; dynamic
efforts were `low`, `medium`, `high`, and `xhigh`. Direct
`session/set_model(grok-4.6)` and `session/set_model(grok-4.5)` both returned a
successful result. Three `model_changed` notifications were observed in wire
order: `grok-4.6/high`, `grok-4.6/high`, then `grok-4.5/high`. The legacy
`session/set_config_option(model, spur-probe)` returned `-32601`; it did not
create semantic native evidence.

Kiro, observed at `2026-09-01T21:38:08.640Z`:

```bash
python3 scripts/probe_acp_capabilities.py \
  --command /usr/bin/env \
  --args "kiro-cli acp" \
  --label kiro-2.20.2-post \
  --cwd /tmp \
  --out /tmp/spur-acp-post.J9b5qf/kiro.jsonl \
  --report /tmp/spur-acp-post.J9b5qf/kiro.report.json \
  --probe-vendor-rpc \
  --always-approve \
  --init-timeout 30 \
  --session-timeout 45 \
  --preamble-timeout 1 \
  --quiet
```

Result: exit `0`; the live catalog contained exactly 9 models
(`auto`, `claude-haiku-4.5`, `claude-sonnet-4`, `claude-sonnet-4.5`,
`deepseek-3.2`, `glm-5`, `minimax-m2.1`, `minimax-m2.5`, and
`qwen3-coder-next`), 3 modes (`kiro_default`, `kiro_guide`, and
`kiro_planner`), and 25 commands. Direct calls accepted
`session/set_mode(kiro_planner)`, `session/set_mode(kiro_default)`, and
`session/set_model(claude-sonnet-4.5)`. Each advertised `optionsMethod`
returned `-32601 Method not found`:

- `_kiro.dev/commands/agent/options`
- `_kiro.dev/commands/model/options`
- `_kiro.dev/commands/prompts/options`

Those three method rejections remain method-level evidence and do not reject
the semantic model capability.

## Published fixtures

| Fixture | Fixture digest | Frames | Claims |
|---|---|---:|---:|
| `scripts/fixtures/acp_capabilities/grok-1.0.13.fixture.json` | `sha256:5d8cfc7e6b5a219e6a237a310d149d5f7a1aa587601a6d4987e4b3774cc78845` | 25 | 112 |
| `scripts/fixtures/acp_capabilities/kiro-2.20.2.fixture.json` | `sha256:79f836fa4d0324eb0c710bb68482b2c6ed0ab83d995373703809df2140302bf5` | 22 | 50 |

Each checked-in file is the fixture object, serialized with sorted keys and a
trailing newline. The fixture digest covers the sanitized identity, ordered raw
ledger, digest-indexed payloads, and normalized claims.

## Deterministic reducer replay

A temporary Rust replay harness linked directly to the current public
`spur_acp::capability_evidence::reduce_capability` function. It validated the
fixture schema/digest, every frame digest, every payload reference, and every
claim raw-digest reference before reducing semantic capability keys. It then
replayed each fixture twice: first with advertisement records only, then with
the paired accepted semantic record included. The temporary harness source
SHA-256 was `c06f37fbc64f6781864a37ac7d0bf186955215171ea8a883e0bd36995412c27c`
and its manifest SHA-256 was
`e3993fd07caabba7eddcc24dbbffbfb773850a985a0cdf00d0e83efd6485828a`.

Exact invocations (repeated as `run1` and `run2` for each provider):

```bash
SPUR_REMOTE=0 CARGO_TARGET_DIR=/tmp/spur-acp-fixture-replay.J9b5qf/target \
  scripts/spur-cargo run \
  --manifest-path /tmp/spur-acp-fixture-replay.J9b5qf/Cargo.toml \
  --quiet -- grok \
  scripts/fixtures/acp_capabilities/grok-1.0.13.fixture.json

SPUR_REMOTE=0 CARGO_TARGET_DIR=/tmp/spur-acp-fixture-replay.J9b5qf/target \
  scripts/spur-cargo run \
  --manifest-path /tmp/spur-acp-fixture-replay.J9b5qf/Cargo.toml \
  --quiet -- kiro \
  scripts/fixtures/acp_capabilities/kiro-2.20.2.fixture.json

cmp /tmp/spur-acp-fixture-replay.J9b5qf/grok.run1.json \
    /tmp/spur-acp-fixture-replay.J9b5qf/grok.run2.json
cmp /tmp/spur-acp-fixture-replay.J9b5qf/kiro.run1.json \
    /tmp/spur-acp-fixture-replay.J9b5qf/kiro.run2.json
```

Both `cmp` commands exited `0`. Stable replay-output SHA-256 values were:

| Provider | Run 1 | Run 2 |
|---|---|---|
| Grok | `5f78ab924018e0aca2016e4c7af3d06917539766e311cc226b2886d4a703bb37` | `5f78ab924018e0aca2016e4c7af3d06917539766e311cc226b2886d4a703bb37` |
| Kiro | `89fbd47cde1eeb94e3e1160ef81d03054acf0929eef041fc778452c3c615ff1a` | `89fbd47cde1eeb94e3e1160ef81d03054acf0929eef041fc778452c3c615ff1a` |

Reduced routes:

| Provider | Capability | Evidence included | Route | Choice evidence |
|---|---|---|---|---|
| Grok | `Model/model` | vendor advertisement only | `PromptOnly` | 2 advertised |
| Grok | `Model/model` | advertisement + 2 paired accepted `set_model` choices | `NativePreferred` | 2 advertised, 2 accepted |
| Grok | `Effort/reasoning_effort` | vendor advertisement only | `PromptOnly` | 4 advertised, including `xhigh` |
| Kiro | `Model/model` | vendor advertisement only | `PromptOnly` | 9 advertised |
| Kiro | `Model/model` | advertisement + paired accepted `set_model` | `NativePreferred` | 9 advertised, 1 accepted |
| Kiro | `Mode/mode` | standard advertisement only | `PromptOnly` | 3 advertised |
| Kiro | `Mode/mode` | advertisement + paired accepted `set_mode` | `NativePreferred` | 3 advertised, 2 accepted |
| Kiro | `Command/command` | vendor advertisement only | `PromptOnly` | 25 advertised |

This is the required semantic distinction: a vendor catalog is candidate
evidence for prompt routing, not native support; the paired successful setter
response is what adds `AcceptedActiveProbe`/`NativeVerified` evidence and
selects `NativePreferred`.

## Claude Code evidence gap

Claude Code was not reclassified. Its prior authentication failure remains an
`InconclusiveFailure` evidence gap. It is neither proof of support nor proof of
unsupported behavior, and this evaluation makes no Claude route claim.

## Verification

All Rust commands used `scripts/spur-cargo`:

```text
python3 -m unittest scripts/test_probe_acp_capabilities.py -v
  OK — 40 tests

scripts/spur-cargo test -p spur-acp capability_evidence
  ok — 12 passed, 0 failed

scripts/spur-cargo test -p spur-acp paired_set_
  exit 0 — paired model/mode semantic-binding filters

scripts/spur-cargo test -p spur-acp rejected_set_model_is_semantic_but_options_method_rejection_is_not
  exit 0

scripts/spur-cargo test -p spur-acp --test executor_events_roundtrip
  ok — 46 passed, 0 failed

scripts/spur-cargo test -p spur-tui commands::
  exit 0
```

No production code or existing test was modified, and no durable signal was
emitted because live behavior, fixture replay, and reducer output agreed.
