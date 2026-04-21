# Beads Startup Warning Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop showing the false `br (beads) not installed` startup warning when PM integration is unavailable by license or disabled in config.

**Architecture:** Keep the fix local to `spur-core`. Extract the startup-warning predicate into a small helper that considers `.beads/` presence, beads config enablement, PM entitlement, and whether `PmService` is actually attached. Unit-test the helper directly, then route the existing startup branch through it.

**Tech Stack:** Rust 2021, `spur-core`, `spur-acp` config types, `spur-license` feature gate

---

## Task 1: Add Regression Tests

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Write failing unit tests for the startup-warning predicate**

Add tests covering:
- Community tier + `.beads/` present + no PM service => no warning
- Entitled tier + `.beads/` present + no PM service => warning
- Entitled tier + beads disabled in config + `.beads/` present => no warning
- No feature gate + `.beads/` present + no PM service => no warning

- [ ] **Step 2: Run the focused test target to verify RED**

Run: `cargo test -p spur-core startup_warning_gate`
Expected: FAIL because the helper does not exist yet and the old logic still assumes `pm_service == None` means `br` is missing.

## Task 2: Patch the Startup Condition

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Add the minimal helper and wire `run_interactive` through it**

Implement a small pure helper for the predicate and replace the inline `self.pm_service.is_none() && self.repo_root.join(\".beads\").is_dir()` check with it.

- [ ] **Step 2: Re-run the focused tests**

Run: `cargo test -p spur-core startup_warning_gate`
Expected: PASS

- [ ] **Step 3: Run a slightly broader verification pass**

Run: `cargo test -p spur-core`
Expected: PASS
