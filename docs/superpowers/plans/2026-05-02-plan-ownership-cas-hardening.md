# Plan Ownership CAS Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden plan-scoped ownership with CAS, owner leases, active handoff, and stale-owner fencing.

**Architecture:** Build on the MVP owner label gate by adding compare-and-set ownership transfer and owner-token checks on all write paths.

**Tech Stack:** Rust 2021, `spur-mcp`, `spur-pm`, beads (`br` CLI), SQLite/Dolt-backed beads internals as exposed by project APIs.

---

## Scope

- [ ] Add CAS mutation support to the beads adapter.
- [ ] Add owner token and lease labels to initial acquisition.
- [ ] Add token-fenced write checks to dispatch, review, merge, and signal mutation paths.
- [ ] Add owner heartbeat renewal.
- [ ] Add active handoff request and `plan-handoff-ready` audit.
- [ ] Add force reclaim with explicit user confirmation.
