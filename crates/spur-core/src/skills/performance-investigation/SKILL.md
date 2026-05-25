---
name: performance-investigation
description: Use when investigating slow code, high resource use, latency regressions, throughput drops, or performance-sensitive behavior before proposing optimizations.
role: worker
---

# Performance Investigation

## Overview

Performance work starts with measurement, not guesses. Identify the user-visible symptom, capture a baseline, isolate the hot path, then make the smallest change that moves the measured bottleneck.

## Process

1. Define the metric: latency, throughput, CPU, memory, allocation rate, I/O, lock contention, or render time.
2. Reproduce the problem with a repeatable command, trace, benchmark, fixture, or manual workflow.
3. Record a baseline before editing code. Include the command, dataset size, environment, and observed value.
4. Profile or instrument the suspected path. Prefer existing project tooling; otherwise add temporary targeted timing around boundaries.
5. Explain the bottleneck with evidence before changing code.
6. Make one focused optimization. Avoid broad rewrites unless the measurements prove the design is the bottleneck.
7. Re-run the same measurement and relevant correctness tests. Report before/after numbers and any tradeoffs.

## Guardrails

- Do not optimize code that is not on the measured hot path.
- Do not compare measurements from different datasets, build modes, machines, or feature flags.
- Do not keep diagnostic logging unless it is intentionally useful for future operation.
- Preserve behavior first; a faster incorrect path is a regression.

## Reporting

Summaries should include:

- Baseline command and result
- Profiling evidence or instrumentation result
- Change made
- After command and result
- Correctness verification
