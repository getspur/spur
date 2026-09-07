# Solver receipt retention

Persisted solves are a bounded repository-local handoff cache, not a permanent
audit archive. Beads remains the collaboration record.

## Contract

- Payload quotas remain 512 receipts and 64 MiB in total. Metadata is separate
  from the payload budget; eviction history retains at most 512 intents.
- New receipts receive unique increasing insertion sequences under the existing
  repository filesystem lock. Eviction selects the oldest **unpinned** sequence,
  regardless of receipt size, solve ID, wall-clock changes, or service restart.
- Existing schema-v1 receipt JSON is unchanged. On first adoption, legacy files
  are ordered by filesystem mtime, then path for deterministic ties. Exact
  historical insertion order cannot be recovered from tied legacy timestamps.
- Pins survive restarts and consume the same quotas as unpinned receipts. The
  store preflights the entire victim set. If eligible victims cannot free enough
  space, the write fails without deleting any existing receipt or updating
  retention metadata. Oversized new payloads are also refused without eviction.
- Corrupt, unsupported, missing-in-an-existing-marker, or symlinked retention
  metadata fails closed for persistence and pin mutations. Ordinary retrieval
  of an existing valid receipt still works.

## Protect evidence explicitly

After persisting a solve, use the MCP tool:

```json
{"solve_id":"sol_0123456789abcdef","pin":true}
```

Pass this to `get_solve_result`. It validates and returns the receipt while
pinning it under the eviction lock. Repeat calls are idempotent. Omit `pin` for
read-only retrieval; use `"pin":false` to release protection after review.
Rust callers can use `SolverService::set_solve_result_pin`.

Pin promptly: separate persist and pin operations are **not atomic**, and
pinning cannot recover a receipt already evicted. Plan tasks are not
automatically discovered or pinned. A pin is shared repository state, not a
per-reviewer reference count: release it only when all consumers have finished.
If all useful capacity is pinned, new persistence reports a quota error rather
than discarding protected evidence. Retain essential proof inputs/results in
the collaboration record or an independently managed archive as well.

## Recovery and diagnostics

Private `.spur/solver/.retention/state.json` holds sequences, pins, and bounded
eviction intents. The first complete metadata directory is atomically published
from a synced temporary directory; subsequent snapshots use atomic file writes.
The solver parent directory is synced before the first eviction on Unix.

An intent is durably recorded **before** deleting a selected victim. It records
the victim ID/sequence, incoming solve ID, selection time, and count/byte pressure.
For a missing receipt whose intent is still retained, `get_solve_result` reports
that selection and its reason using the resource-not-found error code. Absence
without retained history remains an ordinary not-found result; it does not prove
why the receipt disappeared. Intent history is not an unlimited deletion log.

The operation is not an all-or-nothing transaction across receipt files. An I/O
failure or crash after intent publication can leave some unpinned victims
deleted without publishing the incoming receipt. Surviving entries retain their
original sequences; unused reservations are removed on the next mutation and
their sequence numbers are never reused. Pins are never selected as victims.
Unpublished atomic temporary files/directories may remain after a crash.

The metadata **directory** also guards rolling upgrades: older ring writers
reject the non-regular entry during inventory before deleting anything. Upgrade
all solver-writing processes before resuming persistence. Old read-only clients
can still read unchanged schema-v1 receipts. Do not remove the directory to
bypass a writer error: doing so discards protection and ordering information.

## Verification

```sh
scripts/spur-cargo test -p spur-solver
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-solver --all-targets -- -D warnings
scripts/spur-cargo fmt -p spur-solver --check
```

`tests/retention.rs` covers mixed-size eviction, insertion order after clock
changes/reopen, explicit MCP pins, non-destructive count/byte refusal, metadata
safety, the legacy-writer guard, and eviction diagnostics. These tests use
isolated temporary repositories, not the live solver cache.
