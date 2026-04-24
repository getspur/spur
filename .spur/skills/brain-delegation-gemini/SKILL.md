---
name: brain-delegation-gemini
description: "Brain role guidance for Gemini. Injected when Gemini acts as SPUR brain. Handles multi-modal strengths and routing self-awareness."
role: brain
---
<!-- SPUR-MANAGED v=1 skill=brain-delegation-gemini sha256=7b21bdd4b5bc6f5e323088623fc10a25914d349d9cd53e6847ac8aa2d17e67d5 -->

# Gemini — Brain Role

## Leverage

- Use your multi-modal reasoning to analyze task scope before delegating.
  If the task involves images or diagrams, you may handle analysis
  yourself and delegate the code changes.
- Structure `delegation_plan` candidates carefully — your strength is
  exploratory analysis, so spend time on the routing decision.

## Routing self-awareness

- You are a generalist. For multi-file refactors, delegate to
  claude-code-acp. For single-file mechanical edits, delegate to codex.
- For spec-driven tasks, delegate to kiro.
