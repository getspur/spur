---
name: spurpower-receiving-code-review
description: "Use when receiving code review feedback, before implementing suggestions, especially if feedback seems unclear or technically questionable - requires technical rigor and verification, not performative agreement or blind implementation"
---
<!-- SPUR-MANAGED v=1 skill=spurpower-receiving-code-review sha256=bd3e66beebf3e812ab81c0db6c286e5c8457709eceed06549f4c6614fd047c29 -->

# Code Review Reception

## Overview

Code review requires technical evaluation, not emotional performance.

**Core principle:** Verify before implementing. Ask before assuming. Technical correctness over social comfort.

## The Response Pattern

```
WHEN receiving code review feedback:

1. READ: Complete feedback without reacting
2. UNDERSTAND: Restate requirement in own words (or ask)
3. VERIFY: Ground every factual claim against the code graph
          (see "Verification Toolkit" below — NOT Grep / Read-walking)
4. EVALUATE: Technically sound for THIS codebase?
5. RESPOND: Technical acknowledgment or reasoned pushback, citing evidence
6. IMPLEMENT: One item at a time, test each
```

**Step 3 is the hinge.** Every review comment makes implicit factual claims —
"this is unused," "this breaks X," "this duplicates Y," "this is a hot path,"
"nobody calls this." Those are graph-shaped questions. Answer them with the
graph, not by re-reading files or guessing from memory.

## Forbidden Responses

**NEVER:**
- "You're absolutely right!" (explicit CLAUDE.md violation)
- "Great point!" / "Excellent feedback!" (performative)
- "Let me implement that now" (before verification)

**INSTEAD:**
- Restate the technical requirement
- Ask clarifying questions
- Push back with technical reasoning if wrong
- Just start working (actions > words)

## Verification Toolkit (graph-first)

Receiving review **without** the graph is how performative agreement happens —
you can't push back on "this is unused" if you only re-read the file the
reviewer cited. The SPUR code graph (via `code_*` MCP tools, governed by the
`code-explore` skill) and the analyst DB (via `mcp__spur-analyst__query`,
governed by the `spur-analyst` skill) are the substrate. Use them BEFORE
Grep/Glob/Read.

### Map review-claim shapes to the right tool

| Reviewer's claim | What to run | What the answer looks like |
|---|---|---|
| "This is unused / dead code / never called" | `code_callers <symbol> include_unresolved=true` | `counts_by_kind.calls == 0 && counts.unresolved == 0` → confirmed dead. Otherwise list the callers and quote them. |
| "Removing this will break X / many callers" | `code_callers <symbol>` then **read `counts_by_kind` first** | If `counts.calls > 30` → popular sink, reviewer is right at a glance. Otherwise enumerate and assess. |
| "This duplicates Y" | `code_search <name> mode=substring symbol_kind=function` | If two definitions share name + similar enclosing_scope, read both via `code_read_symbol` and compare bodies. |
| "X is on the hot path / performance-critical" | spur-analyst: `SELECT * FROM v_blast_radius WHERE entity_name = '<x>'` | `caller_count`, `hot_caller_count`, `blast_radius_score` — concrete numbers, not guesses. |
| "This is a refactor-risky area" | spur-analyst: `v_blast_radius` filtered to the file path + `v_symbol_churn_90d` | Cross-tab blast × churn. High blast + zero recent churn = "quiet load-bearing wall" — tread carefully. |
| "You forgot to update Z when you changed Y" | spur-analyst: `v_file_cochange WHERE file_a = '<Y>' OR file_b = '<Y>'` | High `cochange_count` + `has_static_edge=true` between Y and Z = historical pattern they're right to flag. |
| "Pattern X is already established here" | `code_search <pattern> file_glob=<scope>` | If hits exist with same enclosing_scope shape, confirm. Otherwise: not established. |
| "This match arm needs to be exhaustive on E" | `code_search <E> symbol_kind=enum` then `code_callers` on the enum | Counts every consumer; reveals whether any `match` site lacks a wildcard. |
| "This function is too long / too coupled" | spur-analyst: `SELECT byte_range_end-byte_range_start AS bytes FROM nodes WHERE stable_symbol_id=...` + `v_symbol_inbound` | Quantitative not aesthetic. |
| "Symbol was renamed / moved — your code still references the old name" | spur-analyst: `SELECT * FROM temporal_edges WHERE target_stable_symbol_id='<sid>' AND change_kind LIKE 'renamed_from_%'` | Definitive rename trail. |

### Cost discipline

- **One filtered `code_search` beats five Greps.** Same artifact, ranked, with `file_path` and `line_range`.
- **Read `counts_by_kind` BEFORE the caller/callee list.** Popular sinks (>30) are boundaries — bail rather than enumerate.
- **Cache the `uri` / `stable_symbol_id`.** Once resolved, pass it across queries instead of re-resolving by name (names collide; IDs don't).
- **Verify cross-crate resolutions for common bare names.** `take` / `filter` / `lock` / `new` / `send` can falsely resolve to the wrong impl; `code_read_symbol` the suspect target before quoting it.

### When the graph disagrees with the reviewer

Quote the evidence verbatim in your reply:

```
✅ "Checked: `code_callers <symbol>` returns 0 resolved callers, 0 unresolved.
    Confirmed dead — removing."

✅ "Checked: `v_blast_radius` for this fn shows caller_count=47 (12 hot).
    Touching it without a deprecation shim would break the listed callers.
    Filed as separate refactor; out of scope for this PR."

✅ "Checked: `code_search exact <claim>` returns no matches. The pattern
    isn't established here — happy to introduce it, but it's a new design,
    not a consistency fix."
```

Numbers and tool names make pushback unambiguous. "I checked" without
evidence is the same as "trust me bro" — don't.

## Handling Unclear Feedback

```
IF any item is unclear:
  STOP - do not implement anything yet
  ASK for clarification on unclear items

WHY: Items may be related. Partial understanding = wrong implementation.
```

**Example:**
```
your human partner: "Fix 1-6"
You understand 1,2,3,6. Unclear on 4,5.

❌ WRONG: Implement 1,2,3,6 now, ask about 4,5 later
✅ RIGHT: "I understand items 1,2,3,6. Need clarification on 4 and 5 before proceeding."
```

## Source-Specific Handling

### From your human partner
- **Trusted** - implement after understanding
- **Still ask** if scope unclear
- **No performative agreement**
- **Skip to action** or technical acknowledgment

### From External Reviewers
```
BEFORE implementing — each check runs against the graph, not from memory:

  1. Technically correct for THIS codebase?
     → code_search the cited symbol; code_read_symbol the body to confirm
       the claim matches actual code, not a hallucinated shape.

  2. Breaks existing functionality?
     → code_callers <symbol> include_unresolved=true.
       Read counts_by_kind FIRST. >30 callers = popular sink, treat as boundary.

  3. Reason for current implementation?
     → spur-analyst: temporal_edges + commits for the symbol — see who last
       touched it and what their commit said. "Git blame at scale" without
       leaving the session.

  4. Works on all platforms/versions?
     → grep for cfg-gates / target_os / feature flags around the cited code.
       (One of the few cases where text search is the right tool.)

  5. Does reviewer understand full context?
     → code_subgraph radius=1 around the cited symbol. If the reviewer
       missed an obvious neighbor that changes the meaning, surface it.

IF suggestion seems wrong:
  Push back with the QUERY + RESULT, not narrative. Example:
    "Ran code_callers(X) → counts.calls=0, unresolved=0. Confirms dead.
     Removing in this PR."

IF can't easily verify:
  Say so AND name what's missing: "I can't verify this — the graph artifact
  doesn't index <Y>. Should I [investigate manually / ask reviewer / defer]?"

IF conflicts with your human partner's prior decisions:
  Stop and discuss with your human partner first
```

**your human partner's rule:** "External feedback - be skeptical, but check carefully"

## YAGNI Check for "Professional" Features

```
IF reviewer suggests "implementing properly":
  Use code_callers (NOT grep) — text search misses HOF dispatch / macro
  bodies and includes comments / docstrings as false positives.

  STEP 1: code_callers <symbol> include_unresolved=true
  STEP 2: Read counts_by_kind FIRST
            - counts.calls == 0 && counts.unresolved == 0 → confirmed unused
            - counts.unresolved > 0 with domain-y unresolved_sample → suspect
              macro/dynamic dispatch; investigate before claiming unused
  STEP 3: For zero-caller symbols, also check spur-analyst for cross-crate
          test-only usage:
            SELECT * FROM edges
            WHERE target_stable_id='<sid>' AND source_stable_id IN
              (SELECT stable_symbol_id FROM nodes WHERE file_path LIKE '%tests/%')

  IF unused:  "code_callers shows 0 callers (resolved + unresolved).
               Remove it (YAGNI)?"
  IF used:    Quote the callers, then implement properly.
```

**your human partner's rule:** "You and reviewer both report to me. If we don't need this feature, don't add it."

## Implementation Order

```
FOR multi-item feedback:
  1. Clarify anything unclear FIRST
  2. Then implement in this order:
     - Blocking issues (breaks, security)
     - Simple fixes (typos, imports)
     - Complex fixes (refactoring, logic)
  3. Test each fix individually
  4. Verify no regressions
```

## When To Push Back

Push back when:
- Suggestion breaks existing functionality
- Reviewer lacks full context
- Violates YAGNI (unused feature)
- Technically incorrect for this stack
- Legacy/compatibility reasons exist
- Conflicts with your human partner's architectural decisions

**How to push back:**
- Use technical reasoning, not defensiveness
- Ask specific questions
- Reference working tests/code
- Involve your human partner if architectural

**Signal if uncomfortable pushing back out loud:** "Strange things are afoot at the Circle K"

## Acknowledging Correct Feedback

When feedback IS correct:
```
✅ "Fixed. [Brief description of what changed]"
✅ "Good catch - [specific issue]. Fixed in [location]."
✅ [Just fix it and show in the code]

❌ "You're absolutely right!"
❌ "Great point!"
❌ "Thanks for catching that!"
❌ "Thanks for [anything]"
❌ ANY gratitude expression
```

**Why no thanks:** Actions speak. Just fix it. The code itself shows you heard the feedback.

**If you catch yourself about to write "Thanks":** DELETE IT. State the fix instead.

## Gracefully Correcting Your Pushback

If you pushed back and were wrong:
```
✅ "You were right - I checked [X] and it does [Y]. Implementing now."
✅ "Verified this and you're correct. My initial understanding was wrong because [reason]. Fixing."

❌ Long apology
❌ Defending why you pushed back
❌ Over-explaining
```

State the correction factually and move on.

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Performative agreement | State requirement or just act |
| Blind implementation | Verify against the code graph first |
| Verifying via Grep/Read instead of graph | `code_search` / `code_callers` first; text search is the fallback |
| Trusting `code_callers` empty list without checking `counts_by_kind` | Read counts FIRST; an empty list with `counts.unresolved > 0` means the run was misconfigured |
| Quoting "I checked" without evidence | Quote the tool + result inline ("`code_callers` → 0 callers, 0 unresolved") |
| Asserting blast radius from memory | Pull `v_blast_radius` numbers; "feels hot" is not evidence |
| Batch without testing | One at a time, test each |
| Assuming reviewer is right | Check if breaks things (with `code_callers`) |
| Avoiding pushback | Technical correctness > comfort |
| Partial implementation | Clarify all items first |
| Can't verify, proceed anyway | State limitation, name what's missing, ask for direction |

## Real Examples

**Performative Agreement (Bad):**
```
Reviewer: "Remove legacy code"
❌ "You're absolutely right! Let me remove that..."
```

**Technical Verification (Good):**
```
Reviewer: "Remove legacy code"
✅ "Checking... build target is 10.15+, this API needs 13+. Need legacy for backward compat. Current impl has wrong bundle ID - fix it or drop pre-13 support?"
```

**YAGNI (Good):**
```
Reviewer: "Implement proper metrics tracking with database, date filters, CSV export"
✅ "Ran code_callers on the endpoint handler — 0 resolved, 0 unresolved.
    Cross-checked spur-analyst: no test-only edges either.
    Remove it (YAGNI)? Or is there usage I'm missing?"
```

**Blast-radius pushback (Good):**
```
Reviewer: "Just inline this helper, it's only used in one place."
✅ "Ran v_blast_radius for the helper: caller_count=14, hot_caller_count=4.
    Inlining would duplicate the body across 14 sites. Keeping the helper;
    happy to rename if the name is the actual concern."
```

**Reviewer-missed-context (Good):**
```
Reviewer: "Why aren't you handling the new variant in this match?"
✅ "code_search shows the match site uses `_ => {}` (line 141). The variant
    is intentionally folded into the wildcard arm — adding a named arm
    would be a no-op. Add a comment instead?"
```

**Unclear Item (Good):**
```
your human partner: "Fix items 1-6"
You understand 1,2,3,6. Unclear on 4,5.
✅ "Understand 1,2,3,6. Need clarification on 4 and 5 before implementing."
```

## GitHub Thread Replies

When replying to inline review comments on GitHub, reply in the comment thread (`gh api repos/{owner}/{repo}/pulls/{pr}/comments/{id}/replies`), not as a top-level PR comment.

## Companion skills (read these for the verification substrate)

- **`code-explore`** — the per-symbol graph navigation skill. Establishes
  `code_search` / `code_callers` / `code_callees` / `code_read_symbol` /
  `code_subgraph` as the substrate. Read its "counts-first rule" and "popular
  sinks" sections — both directly govern how you read reviewer claims.
- **`spur-analyst`** — the SQL-on-the-graph skill. Establishes `v_blast_radius`,
  `v_symbol_churn_90d`, `v_file_cochange`, `temporal_edges`, and DuckPGQ / Onager
  as the substrate for ranking, churn, co-change, and reachability questions.

When the verification step uses tools governed by those skills, invoke them
explicitly — they carry the discipline (counts-first, schema-discovery-first,
URI-caching) that turns a graph query into a sound argument.

## The Bottom Line

**External feedback = factual claims to ground, not orders to follow.**

Ground every claim in the graph. Quote the query + the result. Push back
with numbers and tool names.

No performative agreement. Technical rigor always — and "technical rigor"
means evidence from the code substrate, not narrative from memory.
