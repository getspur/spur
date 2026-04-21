# Beads Startup Copy Tightening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure the startup warning only says `br (beads) not installed` when the `br` binary is actually absent, and use a generic backend-init warning otherwise.

**Architecture:** Keep the change local to `crates/spur-core/src/orchestrator.rs`. Replace the current boolean startup-warning helper with a small warning-kind selector that distinguishes `br` missing from “backend failed to initialize” once the existing entitlement/config gating is satisfied. Render the user-facing copy from that warning kind and keep the PATH probe local to the startup branch.

**Tech Stack:** Rust 2021, `spur-core`, `spur-acp` config types, `spur-license` feature gate

---

## Task 1: Add Red Tests For Warning Selection

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Add failing unit tests for warning-kind selection**

Add tests covering:
- Community tier suppresses all beads startup warnings
- Entitled tier + `.beads/` + no PM service + `br` missing => `not installed` warning
- Entitled tier + `.beads/` + no PM service + `br` present => generic backend-init warning
- Beads disabled in config suppresses warnings
- `pm_service_available = true` suppresses warnings

- [ ] **Step 2: Run the focused test target to verify RED**

Run: `cargo test -p spur-core beads_startup_warning`
Expected: FAIL because the new warning selector / copy helpers do not exist yet.

## Task 2: Implement Warning-Kind Selection

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Add the warning-kind enum, copy renderer, and startup branch selection**

Implement a small helper that returns:
- no warning when license/config/service state says beads startup guidance is irrelevant
- `BrNotInstalled` when startup guidance applies and `br` is absent
- `BackendUnavailable` when startup guidance applies and `br` is present

- [ ] **Step 2: Re-run the focused tests**

Run: `cargo test -p spur-core beads_startup_warning`
Expected: PASS

- [ ] **Step 3: Run the full crate verification**

Run: `cargo test -p spur-core`
Expected: PASS
