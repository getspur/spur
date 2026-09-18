# Grok ACP terminal and continuation evaluation

Issue: bd-3enge. Installed CLI: Grok 1.0.5 (5115b46bc909).

## Evidence and design

The generic capability probe advertises filesystem and terminal support but
implements only permission callbacks. A live harmless `printf` on 2026-09-18
produced two `terminal/create` requests, both rejected by the probe with -32601.
Grok returned `end_turn` with an explanation; this is not evidence of a hung
session. The probe currently sends only one prompt.

Grok still packs `/bin/bash -lc "…"` into `command`, omitting `args`.
SPUR already normalizes that exact provider-specific shape in
`crates/spur-acp/src/connection/native.rs`. Test strict ACP argv and the existing
SPUR compatibility policy separately. Do not widen production shell parsing
without a reproduced mismatch.

Add opt-in real terminal callbacks to the generic probe, with honest default
capabilities, asynchronous waiting, bounded output, and child cleanup. Add
repeatable follow-up prompts in the same session and per-turn observations.
Keep the first `prompt_result` and existing capability artifact contract.
Do not advertise filesystem callbacks that the probe does not implement.

## Implementation and evaluation order

1. Add regression coverage for terminal callback lifecycle, nonzero exit,
   spawn failure, packed Grok argv, concurrent wait/kill, and continuation.
2. Implement the smallest probe changes and run the existing Python suite.
3. Run harmless live strict/compatibility cases: successful shell, failed
   command, same-turn recovery, next prompt, and multiple shell commands.
4. Validate observed finite traces with catalog workflow rules. A finite
   trace verifies only that observation, not unbounded liveness or fairness.
5. Record results and remaining runtime limitations; fix production only if
   evidence identifies a production defect. Commit only this task's files.

Runtime tests and solver results are complementary. Live probes run in an
isolated temporary directory with no repository reads or edits requested.
