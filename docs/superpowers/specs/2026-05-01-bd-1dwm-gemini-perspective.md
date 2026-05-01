# bd-1dwm — Architect Perspective (gemini)

## Recommended approach
Continuous Cherry-Pick Integration (A refined hybrid of A & C)

## Why
The goal is to eliminate late-stage integration conflicts while giving workers the latest state. A true DAG base (Option B) is academically pure but isolates parallel tasks until `merge_plan`, delaying conflict discovery. By incrementally building the final integration branch—cherry-picking novel commits of each approved task onto a single `plan-integration/{plan_id}` branch—workers branch from a state that perfectly mimics the eventual plan merge.

## Reframe / 5th option (if any)
The fatal flaw in Option A as proposed is the "fast-forward" assertion. In a DAG with parallel tasks, sibling branches diverge from their common ancestor. You cannot fast-forward them into a single integration branch. Option E (Continuous Cherry-Pick): Maintain a `plan-integration/{plan_id}` branch. When a task is approved, `git cherry-pick task_base..task_tip` onto it. New tasks branch from this integration tip, effectively linearizing the DAG incrementally rather than deferring integration to the end of the plan.

## Concurrency + retry hazards
Option A (FF-only) completely breaks when parallel siblings M1 and M2 are approved—the second one will fail to FF. Under Option E, if task N fails and retries, its new base will implicitly include any parallel tasks (e.g., P) approved in the interim. This shifting base is a *feature*, not a bug: it forces the retrying worker to resolve integration conflicts early, exactly matching the state `merge_plan` would encounter later, rather than working off a stale snapshot.

## Critique of C specifically
Option C (cherry-picking prior tips into the worker base, then cherry-picking again at plan end) introduces severe commit identity drift. If a worker branch is a fork of cherry-picks, `merge_plan` will struggle to isolate the worker's *actual* contribution from the cherry-picked history unless we strictly track `task.base..task.tip`. If not tracked perfectly, double cherry-picking duplicates commits, litters history with empty patches, and risks silent clobbering when identical changes are applied via differing SHAs.

## Where I agree/disagree with kimi (A+D)
I strongly agree with kimi on implementing D (Detect and signal) immediately as a cheap safety net. However, I disagree with their MVP for A. An FF-only integration branch is a linear constraint misapplied to a DAG; it will violently reject parallel approvals. We must embrace linearization via cherry-picking (Option E) to keep the architecture aligned with `merge_plan`'s ultimate behavior.

## Sharpest tradeoff
By linearizing the DAG at dispatch time via an integration branch, we leak state: tasks can accidentally rely on the code of undeclared parallel tasks that happened to finish first, reducing strict functional isolation in exchange for integration safety.
