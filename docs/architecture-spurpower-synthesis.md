# Spurpower: The Hybrid Architecture Synthesis

**Date:** 2026-04-19
**Status:** Approved
**Authors:** L9 Rust Engineering / MCTS Synthesis

## 1. Executive Summary

This document outlines the architectural paradigm shift from **Superpowers** (a prompt-based, discipline-enforcing markdown framework) to **Spurpower** (a Rust-based, MCP-driven orchestration engine).

Through rigorous Monte Carlo Tree Search (MCTS) and Iceberg Framework analysis, we identified a critical flaw in relying purely on Large Language Models (LLMs) for complex, multi-step workflows: **LLMs rationalize.** When placed under pressure (time, context bloat, sunk cost), LLMs will ignore prompt-based instructions to skip steps like Test-Driven Development (TDD) or Code Review.

**The Iron Law of Spurpower:**
*Discipline cannot be prompted; it must be compiled.* 

If a process step is mandatory (e.g., code review, parallel execution, dependency waiting), it must be a structural node in a Directed Acyclic Graph (DAG) enforced by Rust, not a sentence in a markdown file.

## 2. The Iceberg Framework Analysis

We analyzed the failures of pure prompt-based orchestration:

*   **Level 1: Events (What happens?)** Agents skip tests, hallucinate success, and fail to track dependencies over long sessions.
*   **Level 2: Patterns (What trends emerge?)** As context length grows, prompt adherence drops. Agents optimize for immediate user satisfaction over systematic rigor.
*   **Level 3: Underlying Structures (What causes this?)** We were trying to map **Micro-loops** (tactical coding and debugging) into a **Macro-orchestrator** (the LLM's context window).
*   **Level 4: Mental Models (The Paradigm Shift)** 
    *   *Old Belief:* "LLMs are unreliable, so we must write stricter markdown prompts."
    *   *New Belief:* "LLMs are incredible **tactical solvers** when constrained, but disastrous **long-term strategists and state managers**."

## 3. The Hybrid "Sweet Spot" Architecture

To prevent over-engineering the orchestrator (e.g., turning a 3-second TDD loop into a slow, multi-agent DAG), Spurpower splits responsibilities into two distinct planes:

### A. The Control Plane: Macro-Discipline (Spur / Rust / MCP)
Rust handles state, routing, and dependencies. The LLM cannot "sweet talk" its way out of these constraints.
*   **`subagent-driven-development` ➔ `submit_plan` & `execute_epic`:** The orchestrator agent writes an epic in Beads and calls `submit_plan`. Spur automatically builds the dependency graph.
*   **`dispatching-parallel-agents` ➔ `delegate_parallel`:** Spur natively spins up concurrent worker threads, isolating their contexts.
*   **`requesting-code-review` ➔ `review_task` (The Review Gate):** When a worker finishes, Spur moves the task to `awaiting_review`. A downstream task *cannot* start until a human or `code-reviewer` agent calls `review_task(approve)`. 

### B. The Data Plane: Micro-Discipline (Superpowers / Markdown)
Markdown files (`SKILL.md`) handle tactical execution. These are injected into the isolated worker nodes.
*   **`test-driven-development` (TDD):** Runs rapidly *within* the implementer's single session. The orchestrator doesn't micromanage the Red-Green-Refactor loop.
*   **`systematic-debugging`:** Guides the worker to trace root causes locally using `grep_search` and `run_shell_command` before escalating failures to the orchestrator.
*   **`verification-before-completion`:** Prevents the worker from calling `check_delegation_status(complete)` without fresh terminal evidence.

## 4. Implementation Strategy

To realize this architecture, the `spur` workspace implements the following:

1.  **Canonical Skills in Core (`crates/spur-core/src/skills/`):** The foundational `SKILL.md` files reside directly in the Rust core as the source of truth.
2.  **Auto-Provisioning (`spur init`):** When initializing a workspace or instantiating multi-type agent workers (`implementer`, `spec-reviewer`), the CLI automatically synchronizes these canonical skills into the specific agent configuration directories (e.g., `.spur/skills/`).
3.  **MCP Tool Enforcement:** Tools like `submit_plan` and `review_task` strictly enforce the DAG state transitions, completely abstracting the complexity away from the orchestrator agent's prompt.
