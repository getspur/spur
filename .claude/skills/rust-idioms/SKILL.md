---
name: rust-idioms
description: >
  Mandatory guidance for writing, refactoring, and reviewing Rust code.
  Handles borrow checker discipline, async patterns, idiomatic error handling,
  and integration with Spur's strict verification rules.
role: worker
activation: always
---

# Rust Idioms & Development Workflow

## Overview

Rust is strict. AI agents often try to bypass the compiler by guessing or using anti-patterns (like aggressive cloning) rather than understanding the architecture. 

**Core principle:** Understand ownership and lifetimes first. The compiler is your partner, not an obstacle to silence.

## The Iron Laws of Rust AI Coding

```
1. NEVER use .unwrap() or .expect() in production code.
2. NEVER blindly add .clone() to silence a borrow checker error.
3. ALWAYS run `cargo check` after small changes (Check-Driven Development).
```

## 1. Check-Driven Development (CDD)

Rust code cannot be written in massive blocks without verification. 

**The Loop:**
1. Write a small logical unit (10-30 lines).
2. IMMEDIATELY run `cargo check` or `cargo clippy`.
3. Fix errors BEFORE writing the next logical unit.

**Why:** Cascading borrow checker errors are impossible to fix if you write 100 lines at once. 

## 2. Borrow Checker & Lifetime Discipline

When you encounter `rustc` errors like `E0597` (borrowed value does not live long enough) or `E0499` (cannot borrow as mutable more than once at a time):

**DO NOT:**
- Immediately add `.clone()`
- Immediately wrap in `Arc<Mutex<T>>`
- Change `&T` to `T` without architectural justification

**DO (Applying Systematic Debugging):**
1. **Read the compiler `help:` note.** It often tells you exactly what is wrong.
2. **Trace Ownership:** Who *should* own this data? Should it be borrowed `&T` or passed by value `T`?
3. **Fix the Architecture:** Often, lifetime issues mean the struct design or function signature is flawed. Rethink the data flow rather than fighting the compiler.

## 3. Idiomatic Error Handling

**Production Code:**
- Return `Result<T, E>`.
- Use the `?` operator for propagation.
- **Libraries (`spur-core`, `spur-acp`):** Use the `thiserror` crate to define precise, matching enum errors.
- **Applications (`spur-cli`, `spur-tui`):** Use the `anyhow` crate for flexible context propagation.

**Never panicking:**
Unless writing a script explicitly allowed to fail, `unwrap()` and `expect()` are banned. If a state is truly impossible, use `unreachable!("Reason")` with a clear explanation.

## 4. Asynchronous Patterns (Tokio)

When working in `spur-acp` or other async contexts:

- **No Blocking I/O:** Never use `std::fs` or `std::thread::sleep` inside an `async fn`. Use `tokio::fs` or `tokio::time::sleep`.
- **CPU Bound Work:** If you must do heavy synchronous work, wrap it in `tokio::task::spawn_blocking`.
- **Cancellation Safety:** Ensure branches in `tokio::select!` are cancellation-safe.

## 5. Integrating with Spur Workflows

### TDD in Rust (`superpowers:test-driven-development`)

When following the Red-Green-Refactor cycle in Rust:
- **Unit Tests (Testing Internals):** Place these at the bottom of the *same file* you are modifying, inside a `#[cfg(test)] mod tests { ... }` block.
- **Integration Tests (Testing Public API):** Place these in the `tests/` directory at the root of the crate (e.g., `crates/spur-acp/tests/`).
- **Doc Tests:** For public utility functions, include a `# Examples` block in the docstring.

### Verification (`superpowers:verification-before-completion`)

Before claiming a Rust task is complete, you MUST provide evidence from:
1. `cargo check` (Exit 0)
2. `cargo clippy -- -D warnings` (No warnings)
3. `cargo test` (All tests passing)

**Workspace Awareness:**
Spur is a multi-crate workspace. When running verification commands, ensure you target the correct crate to save time, or run from the root if the change spans boundaries.
Example: `cargo test -p spur-acp`

## Red Flags - STOP

- Using `.unwrap()` to save time on error handling.
- Adding `.clone()` because the compiler complained.
- Writing > 50 lines of code without a `cargo check`.
- Claiming "The code should compile now" without running the compiler.

**If you see these, STOP. Revert to discipline.**