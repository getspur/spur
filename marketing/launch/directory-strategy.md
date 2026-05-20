# Directory Strategy — SPUR Launch

*2026-05-20. Companion to `marketing/launch/directory-tracker.csv`. Grounded in `marketing/product-marketing.md` V1.3 (proprietary, no public repo) and the `marketing-directory-submissions` skill.*

## (a) Why we skip OSS-only directories

`product-marketing.md` V1.3 (line 10) is unambiguous: **SPUR is proprietary. Source is NOT publicly available.** The distribution table at line 218 explicitly flags OSS-specific directories as ❌ ("Skip OSS-specific directories — TAAFT-AI is OK; AwesomeLists is not"). Submitting SPUR to `awesome-mcp-servers` or any `awesome-*` GitHub list would (i) get rejected by a maintainer who clicks through and finds no repo, (ii) burn the first-submission advantage if we *do* later open-source select crates per line 103, and (iii) waste the moderator's time, which damages reputation for the eventual ACP-client-crate submission. The MCP Registry is a similar mis-fit by category (it lists MCP *servers*; SPUR is an MCP *client* and orchestrator), not by proprietary status — same outcome, different reason.

## (b) Submission-day vs. ongoing-discovery directories

Two distinct submission rhythms appear in the tracker:

- **Launch-day batched (TAAFT, Futurepedia, DevHunt, Fazier, Toolify, AI Agents Directory):** these are *moment-of-launch* surfaces. Their value is the spike of launch-week traffic and the synchronized backlink burst that helps PH momentum. Submit on launch day in a 2-hour batch.
- **Ship-before-launch (AlternativeTo, SaaSHub, BetaList, StartupBase):** these are *ongoing-discovery* surfaces. AlternativeTo and SaaSHub compound for months via "[competitor] alternative" search queries; the listing needs to be live and indexed *before* launch day so the launch-week spotlight finds an already-ranking page rather than a freshly-submitted one waiting on moderation. BetaList is calendared 2–3 weeks ahead by its own editorial queue.

Mixing the two rhythms is the most common mistake — submitting AlternativeTo on launch day means the listing only starts earning search compounding *after* the launch traffic has dissipated.

## (c) Priority ranking rationale

Priority 1 = launch-blocker (the listing must exist by Day 0). Priority 2 = launch-week. Priority 3 = launch-month. Priority 4 = post-launch deferred (gated on customer count, reviews, or DR verification). Priority 5 = skip or verify-only. DR-to-effort ratio breaks ties: Futurepedia (P1) outranks AI Tools Directory (P4) at similar effort because DR ~70 vs. ~30 is a 40× link-equity difference at the same cost in human time.
