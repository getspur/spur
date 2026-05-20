# Cosine (Genie)

*Profile date: 2026-05-20. INDIRECT competitor — autonomous-agent platform with proprietary fine-tuned coding models. Same schema notes as `devin.md`.*

## Identity

- **Name:** Cosine (company) / Genie (agent product line) / Lumen (their production-first coding model family).
- **Official site:** https://cosine.sh
- **Pricing:** https://cosine.sh/pricing
- **Research:** https://cosine.sh/research
- **Status:** Genie 2.0 marketed as "fastest, lightest, most capable autonomous software engineer" ([Genie 2.0 blog](https://cosine.sh/blog/genie-autonomous-software-engineer)).

## Headline pitch

> "AI engineering you control. Hand off complex coding tasks without sacrificing maintainability or visibility." — [cosine.sh homepage](https://cosine.sh)

Explicit "beyond the copilot era" framing: not a code-completion sidekick — a production-grade autonomous engineer with enterprise governance.

## Agent model

- **Multi-agent orchestration**, particularly in the terminal product: "multi-agent orchestration from the command line" ([cosine.sh homepage](https://cosine.sh)).
- Underneath, Genie itself is a fine-tuned coding model (Genie 1 = fine-tuned GPT-4o per [VentureBeat](https://venturebeat.com/programming-development/move-over-devin-cosines-genie-takes-the-ai-coding-crown); Genie 2 + Lumen are proprietary).
- Workflow loop: retrieve → plan → write → run ([deeplearning.ai Batch coverage](https://www.deeplearning.ai/the-batch/genie-coding-assistant-outperforms-competitors-on-swe-bench-by-over-30/)).

## Architecture

Three deployment surfaces ([cosine.sh homepage](https://cosine.sh)):

- **Desktop app** — local development.
- **Cloud collaboration environment** — shared async sessions.
- **CLI / terminal** — local-to-remote execution, multi-agent orchestration.
- **Enterprise:** public cloud, dedicated tenant, **air-gapped** — explicitly designed for regulated environments.

## Pricing

From [cosine.sh/pricing](https://cosine.sh/pricing) (2026-05-20):

| Tier | Price | Credits |
|------|-------|---------|
| Hobby | $20 / seat / mo | 5M Cosine Credits / seat / mo |
| Professional | $200 / seat / mo | 60M Cosine Credits / seat / mo |
| Enterprise | Custom | Custom deploy (cloud / VPC / air-gapped), custom model weights, zero data egress |

Top-ups: $20 / 5M credits or $200 / 60M credits.

## Target persona

Enterprise engineering teams in **regulated industries** with strict security/compliance requirements; orgs with **legacy and niche-language codebases** (COBOL, Fortran, Verilog, plus Rust/SQL) — explicit positioning on [cosine.sh homepage](https://cosine.sh).

## Adoption signals

- **SOTA SWE-bench scores (Genie 1, Aug 2024):** 30.1% full / 50.7% Lite / 44% Verified — beating Amazon Q at ~19.75% by the largest margin ever recorded at the time ([deeplearning.ai](https://www.deeplearning.ai/the-batch/genie-coding-assistant-outperforms-competitors-on-swe-bench-by-over-30/), [VentureBeat](https://venturebeat.com/programming-development/move-over-devin-cosines-genie-takes-the-ai-coding-crown)).
- Homepage shows "Trusted by Engineers at" logo strip; specific logos not extracted in this scrape.
- Air-gapped enterprise tier suggests confirmed regulated-industry pipeline.

## Top 3 strengths

1. **Best-in-class benchmark provenance.** Genie 1 set the SWE-bench record by the widest margin in benchmark history at the time. That's a hard-to-fake credential when selling into enterprise eng.
2. **Production-first proprietary models (Lumen).** Cosine controls the model stack, including weights for air-gapped enterprise customers. This is a deeper moat than thin-wrapper competitors.
3. **Niche-language coverage.** COBOL/Fortran/Verilog is a deliberate, well-funded enterprise pitch — the market segment most willing to pay $200/seat.

## Top 3 reasons a SPUR user would still want SPUR

1. **Cosine = proprietary model lock-in; SPUR = brain-swap.** Cosine Credits buy Lumen/Genie inference. If Genie is congested or your task is better-served by Sonnet 4.6 / Codex / GLM, you have no recourse inside Cosine. SPUR's whole architectural bet is that the **agent is interchangeable**, which is exactly the wedge F2 themes #1 and #5 (`themes.md:7-19`, `:67-78`) describe.
2. **Cosine doesn't surface multi-vendor spend.** Cosine Credits are a single-currency abstraction *inside* Cosine. SPUR's pitch is *aggregate* spend across Claude, Codex, Gemini, GLM, local models — the cost-opacity pain (`themes.md:23-34`) is one Cosine can't address by design.
3. **Local control + hackable.** Even Cosine's "desktop app" is a frontend onto Cosine's runtime. SPUR sits on the user's tmux/worktree workflow rather than replacing it. The wedge persona (`themes.md:52-65`) already chose this stack and won't migrate to a Cosine-owned runtime.

## Notes for downstream positioning

- **Cosine is the enterprise/regulated play.** SPUR shouldn't fight there in F4-F8. Cosine wins air-gapped, compliance, niche-language.
- Cosine's "Hobby $20" tier *does* overlap with SPUR's solo-developer persona; pricing parity matters if SPUR ever paywalls the orchestrator.
- The "agent you control" message from Cosine is very close to language SPUR has used internally — pick a different word ("orchestrator", "control tower" per Beefin) to avoid head-on collision.
