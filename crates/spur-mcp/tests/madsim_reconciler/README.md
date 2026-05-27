# madsim reconciler simulations

Status: blocked. The draft harness compiles as a gated integration test, but
`spur-mcp` cannot currently run `Reconciler::run` on madsim without a broader
Tokio replacement. A crate-root `madsim_tokio as tokio` alias fixes
`Reconciler::run` but breaks transitive real-Tokio types in `axum` and
`tokio-util` (`TcpListener`, `JoinHandle`, `JoinSet`).

This harness runs only when both the Cargo feature and cfg are set:

```sh
RUSTFLAGS="--cfg madsim" scripts/spur-cargo test -p spur-mcp \
  --features madsim-sim --test madsim_reconciler
```

Each test has a fixed default seed and prints it on start. To replay a failing
seed:

```sh
MADSIM_TEST_SEED=<seed> RUSTFLAGS="--cfg madsim" scripts/spur-cargo test \
  -p spur-mcp --features madsim-sim --test madsim_reconciler <test_name> -- --nocapture
```

Scenario map:

- `happy_pending_ready_dispatched_awaiting_review_approved_complete`: normal
  ready projection, dispatch, completion collector, approval, and terminal epic
  close.
- `edge_lease_expiry_silent_worker_reclaims_and_redispatches`: expired
  `Dispatched` lease is reclaimed before ready dispatch.
- `edge_setup_conflict_blocks_and_does_not_auto_clear`: predispatch overlay
  conflict moves a task to `BlockedOnSetupConflict` and emits a continuation.
- `edge_terminal_before_dispatch_closes_epic_before_stale_ready_dispatch`:
  terminal epic reconciliation runs before stale ready rows can dispatch.
- `edge_cancel_mid_tick_keeps_durable_dispatch_for_next_recovery`: cancellation
  races an in-flight dispatch send after durable dispatch intent.
- `edge_fast_forward_storm_ticks_without_starvation`: many fast-forward wakes
  still allow at least one tick before cancellation.

The fake PM lives at the `PmLike`/`BeadsAdvanced` boundary. It does not mock git
or filesystem below the reconciler; overlay conflict tests use the existing
predispatch preview strategy.
