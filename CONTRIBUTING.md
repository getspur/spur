# Contributing

## Tests — default vs UAT

`spur-mcp`'s integration tests are split into two tiers: fast in-memory tests
that run on every default invocation, and UAT / external-dep tests that
require `br` (and sometimes `sqlite3`) to be on `PATH`. The slow tier is
gated with `#[ignore]` so the default `cargo test -p spur-mcp` invocation
stays fast and never silently passes 0.

### Run modes

| Command                                              | Runs                              |
| ---------------------------------------------------- | --------------------------------- |
| `cargo test -p spur-mcp`                             | Fast tests only (default)         |
| `cargo test -p spur-mcp -- --ignored`                | UAT / external-dep tests only     |
| `cargo test -p spur-mcp -- --include-ignored`        | Everything                        |

The default invocation should finish well under a minute on a warm build.
If a default `cargo test -p spur-mcp` ever takes >2 min or hangs, that's a
bug — open an issue or annotate the offender with `#[ignore]`.

### Ignore-annotation convention

Use `#[ignore = "<reason>"]` directly above `#[tokio::test]` or `#[test]`.
The `<reason>` string must start with one of the prefixes below so a quick
grep can sort tests by tier.

| Prefix                            | When to use                                                            |
| --------------------------------- | ---------------------------------------------------------------------- |
| `requires br on PATH`             | Test shells out to the `br` binary. Will fail loudly without it.       |
| `requires <other dep>:`           | Test depends on `sqlite3`, a network endpoint, etc.                    |
| `slow integration: <reason>`      | Test is hot-path slow (e.g. measured ≥30s wall) but has no extra dep.  |
| `heavy: <reason>`                 | Test is intentionally large (bulk inserts, stress).                    |

Always append `; run with --ignored` so the message is self-documenting:

```rust
#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn my_test() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
    // ...
}
```

Inside the test body, prefer `assert!(<dep>_available(), "...")` over
`if !<dep>_available() { eprintln!("..."); return; }`. The `assert!` form
turns a missing dep into a hard failure when the test is explicitly
invoked, instead of silently passing 0.
