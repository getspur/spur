# Cursor (Composer / Agent mode)

*Profile date: 2026-05-20. INDIRECT competitor — IDE-native AI with multi-agent / cloud-agent extensions. Same schema notes as `devin.md`.*

## Identity

- **Name:** Cursor (IDE) / Composer 2.5 (agentic feature) / Agents & Cloud Agents (parallel runners).
- **Official site:** https://cursor.com
- **Features:** https://cursor.com/features
- **Pricing:** https://cursor.com/pricing
- **Company:** Anysphere.

## Headline pitch

> "Built to make you extraordinarily productive, Cursor is the best way to code with AI." — [cursor.com/features](https://cursor.com/features)

Composer 2.5 framed as "turn ideas into code" — agentic hand-off inside the IDE.

## Agent model

- **Multi-agent (parallel)** at the Agent layer: "Multiple agents run simultaneously across different tasks" using "their own dedicated computers" ([cursor.com/features](https://cursor.com/features)).
- Composer is the inline-IDE agentic loop; Agents/Cloud Agents are background workers spun off to remote sandboxes.
- Multi-model: Composer 2.5 (proprietary), plus GPT-5.5, Opus 4.7, Gemini 3.1 Pro, Grok 4.3, Auto-select.

## Architecture

- **Hybrid:** desktop IDE (VS Code fork) + cloud agents on Cursor-managed infra.
- Cross-tool integrations: terminal, Slack, GitHub PR reviews, Jira, MS Teams.
- The IDE remains the source of truth; cloud agents are a feature *of* the IDE, not a standalone CLI.

## Pricing

From [cursor.com/pricing](https://cursor.com/pricing) (2026-05-20):

| Tier | Price | Notes |
|------|-------|-------|
| Hobby | $0 | Limited Agent requests + Tab completions, no card |
| Individual | $20 / mo | Extended Agent limits, frontier models, MCPs/skills/hooks, cloud agents, Bugbot (usage-based) |
| Teams | $40 / user / mo | Shared cloud agents, team rules/skills/automations, Security Review agent, SAML/OIDC, plugin marketplace, analytics |
| Enterprise | Custom | Pooled usage, invoice/PO, SCIM, audit logs, Bugbot custom |

Pricing page also mentions "Pro+ for daily agent users" and "Ultra for agent power users" sub-tiers within Individual.

## Target persona

- **Free tier:** student / hobbyist.
- **Individual $20:** the modal IC developer.
- **Teams / Enterprise:** Fortune 500 — explicitly the marketing claim.

## Adoption signals

- **"Over half of the Fortune 500" trust Cursor** ([cursor.com/features](https://cursor.com/features)).
- **NVIDIA:** 40,000+ engineers using Cursor (per features page).
- **Stripe**, Y Combinator portfolio companies named.
- Cursor is widely treated as the **default IDE for AI-assisted coding in 2025-26** — the de-facto incumbent SPUR sits orthogonal to.

## Top 3 strengths

1. **Distribution & default-tool status.** Cursor is *the* AI IDE for the median developer. SPUR will never out-distribute it for editor-bound users.
2. **Tight IDE integration.** Inline diffs, multi-file edits with live cursor, Composer chat inside the editor, Bugbot in PR reviews — the inner-loop experience is hard to beat from outside the editor.
3. **Cloud agents inside an IDE workflow.** Cursor *also* offers parallel cloud agents, but the user never leaves the editor. That's a strong UX even for the multi-agent JTBD SPUR targets.

## Top 3 reasons a SPUR user would still want SPUR

1. **Cursor is editor-centric; SPUR's wedge persona is terminal-centric.** F2 voice-of-customer (`marketing/research/themes.md:43-49`) is explicit: people run 5-10 *CLI agents* (Claude Code, Codex) in tmux/worktrees and need a control tower for that fleet. They have already opted out of "the IDE is the surface." Cursor's UI assumes you live inside the editor.
2. **Cursor's cloud agents lock work into Cursor's runtime + cloud sandbox.** SPUR runs agents on your repo, your shell, your worktrees — same place your tests, hooks, and tools already work. No "it runs differently on Cursor's machine" surprises.
3. **Vendor-lock + opaque per-tier billing.** Cursor's "Pro+/Ultra" sub-tiers and usage-based Bugbot reproduce the **cost-opacity** pain (`themes.md:23-34`) for the multi-agent user. SPUR's status-bar live spend is the antidote, *and* it works across Claude / Codex / Gemini / GLM, not just Cursor's curated model set.

## Notes for downstream positioning

- **Do NOT position SPUR vs Cursor.** Different surface (terminal vs editor), different persona (DIY orchestrator vs IDE user). A "Cursor alternative" frame would lose — Cursor has overwhelming distribution.
- The honest framing: **"SPUR sits next to your editor, not instead of it."** Many SPUR users will keep Cursor open in another window. Treat Cursor as a peer, not a competitor, in vs-page copy.
- Watch Cursor's Cloud Agents roadmap closely — if they ship per-fleet review queues + cross-agent cost ledger, the gap narrows fast.
