# Solver verification: pi SessionStandardizer contract

Z3 4.16 negation proofs for `crates/spur-acp/src/adapter/pi.rs` (2026-09-19).
Each query asserts the NEGATION of one invariant; `unsat` proves it. Positive
controls (violation assert replaced by a tautology) return `sat`, ruling out
vacuity. Run isolated: `z3 -smt2 qN.smt2`.

| Query | Invariant | Result |
|-------|-----------|--------|
| q1 | Cumulative: Σ chunk len ≤ CAP ⇒ emitted raw_output = exact concatenation (3 unrolled steps) | unsat (proof) |
| q2 | Cap bound: oversized chunk ⇒ trimmed output len ≤ CAP | unsat (proof) |
| q3 | Exit footer: ∀e ∈ [1, 2³¹−1] raw_output strictly longer than accumulator | unsat (proof) |
| q4 | Flush: post-terminal_exit chunk accumulates from empty | unsat (proof) |

Out of solver scope (covered by Rust unit tests in `adapter/pi.rs`):
multibyte char-boundary trim (`accumulator_trims_at_cap_on_char_boundary`),
exact footer text, Completed/Failed enum mapping.
