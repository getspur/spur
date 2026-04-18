# SPUR — Pitch Deck

**The orchestration layer between your AI agents and your work.**
**Issue in → PR out — across every agent.**

---

## 1. The Problem

**Your AI agents are smart. Your workflow isn't.**

- **$200/month on Claude Max** — users burn through 5-hour session limits in 90 minutes. Anthropic admitted the issue affects ~7% of Max subscribers during peak hours.
- **55% of developers now use AI agents weekly** — but they manually juggle 2-5 different agent CLIs in separate terminal tabs, copy-pasting context between them.
- **3-5× weekly rate limit hits** — when Claude Code throttles, all work stops. No automatic failover to alternative agents. Flow state destroyed.
- **$0 visibility into AI spend** — teams spending $3,000+/month on AI coding tools have zero cross-agent cost tracking. No team lead can answer "where is our AI money going?"
- **Single-vendor lock-in** — Claude Code subagents are Claude-only. Agent Teams are Claude-only. No native mechanism to delegate to Codex, Kiro, or Gemini based on task fit.

---

## 2. The Solution

**One TUI. Every agent. Intelligent routing.**

- **SPUR** is a Rust-native terminal tool that orchestrates multiple AI coding agents through the Agent Client Protocol (ACP)
- Routes the right task to the right agent at the right time — automatically
- Three interfaces in one binary:
  - `spur watch` — TUI dashboard (at your desk)
  - `spur run` — CLI runner (scripts & CI/CD)
  - `spur serve` — chatbot gateway (Telegram, from your phone)
- Connects to PM tools (Linear, Plane, GitHub Issues) for closed-loop automation
- Tracks cost per agent, per task, per project — the only tool that answers "where is our AI money going?"

**Demo:**
```
$ spur run "fix the auth bypass in jwt.rs"

🧠 Classified: security bug, medium complexity
📊 Scoring: kiro 4.12 → gemini 4.01 → codex 3.58
🔧 Routing to kiro (82% success rate on security bugs)

│ Reading src/auth/jwt.rs...
│ Found: token validation skips expiry check
│ Fix applied. Tests: 14/14 passed

✓ Done in 47s • $0.18 (saved $0.47 vs Claude)
```

---

## 3. How It Works

**Brain decides. Workers execute. SPUR coordinates.**

- **Layer 1 — Classify:** Keywords → file analysis → brain (Gemini, free). Determines task type, domain, complexity. Cost: $0.
- **Layer 2 — Filter:** Checks which agents are installed, authenticated, online, and not rate-limited. Cost: $0.
- **Layer 3 — Score:** Weighted scoring: capability × cost × speed × success_rate × availability. Picks the optimal agent. Cost: $0.
- **Layer 4 — Execute:** Routes to winner via ACP protocol. Streams results to TUI. Cost: agent tokens only.
- **Layer 5 — Learn:** Records outcome in SQLite. Success rates improve over time. Data moat accumulates.

**Supported agents (Day 1, no partnerships needed):**
- Gemini CLI — FREE (1,000 requests/day, 1M token context)
- Kiro CLI — Native ACP, own credits, spec-driven
- OpenCode — BYOK (any LLM provider), 6.5M monthly users
- Claude Code — via `claude -p` wrapper, TOS-compliant
- Codex CLI — fast targeted edits, OpenAI API
- Goose, Mistral Vibe, Kimi CLI, 20+ more via ACP

---

## 4. Market Opportunity

**The agent gold rush needs a pickaxe.**

- **$26.3B** — AI agent orchestration market projected by 2034 (18.8% CAGR)
- **$7.38B** — AI agents market in 2025, growing to $100B+ by 2032
- **6.5M** — OpenCode monthly active developers (multi-agent is mainstream)
- **~90K** — Gemini CLI GitHub stars (free tier most devs don't fully exploit)
- **30+** — ACP-compatible agents in the official registry (protocol is standardizing)
- **55%** — developers use agents weekly; 63.5% for staff+ engineers
- **46%** — "most loved" tool: Claude Code, but rate limits drive active frustration
- **73%** — of engineering teams use AI coding tools daily (up from 41% in 2025)
- **340%** — increase in job postings requiring AI coding tool experience (Jan 2025 → Jan 2026)

**Target market:** Senior/staff engineers and team leads at startups and mid-size companies spending $200-600/month on AI coding tools with zero visibility or cross-agent coordination.

---

## 5. Business Model

**Open core + LTD launch + team subscription.**

**Phase 1: Lifetime Deals (Month 3-9) — Cash for development**
- Community (Free forever) — Open source core, 3 agents, 3 workflows, basic TUI
- Starter LTD ($39 once) — 3 agents, 1 chatbot channel, GitHub Issues
- Builder LTD ($79 once) — Unlimited agents/workflows, Linear/Plane, auto-failover
- Founder LTD ($149 once, 500 seats max) — Everything + future local features + 50% off Team

**Phase 2: Subscriptions (Month 9+) — Recurring revenue**
- Pro ($19/user/month) — replaces Builder LTD for new users
- Team ($39/user/month) — shared cost dashboard, budget caps, smart routing, SSO
- Enterprise (custom annual) — self-hosted, SAML, audit logs, SLA

**Unit economics:**
- Marginal cost per LTD user: ~$0 (binary runs locally, user owns agents)
- LTD phase target: 600 users × $79 avg = ~$47K cash
- Each LTD user generates routing data = acquisition cost for data moat
- Team tier pays for itself if it identifies $40/mo in wasted agent spend per developer

**Revenue projections:**
- Month 6: ~$47K cumulative (LTD cash)
- Month 12: ~$4,800/month MRR
- Month 20: $100K ARR
- Month 24: ~$28,000/month MRR ($336K ARR)

---

## 6. Competitive Landscape

**SPUR occupies the only empty quadrant.**

|  | SPUR | ACPX | OpenClaw | Agent Teams | TUICommander |
|---|---|---|---|---|---|
| Language | **Rust** | TypeScript | TypeScript | TypeScript | Rust+Tauri |
| Interface | **TUI+CLI+Bot** | CLI only | 20+ channels | Terminal | Desktop |
| ACP native | **Yes** | Yes | Embedded | No | No (PTY) |
| Cross-agent | **Yes** | Yes | Yes (via ACPX) | Claude only | Detection only |
| PM integration | **Yes** | No | No | No | No |
| Cost tracking | **Yes** | No | No | Claude only | No |
| Workflow engine | **TOML** | Flows | Skills | TaskCreate | No |
| Pricing | **OSS + LTD** | Free | Free | Included | Free |

**Why SPUR wins:**
- OpenClaw (346K stars) is a general-purpose assistant — SPUR is purpose-built for coding agent orchestration
- ACPX (2.1K stars) is CLI-only — no TUI, no PM integration, no cost tracking
- Claude Agent Teams is Claude-only — doesn't solve "Claude is rate limited"
- Nobody else tracks cost across agents for a team — SPUR's most unique feature

---

## 7. Defensibility

**Code is forkable. Data isn't.**

- **Routing intelligence (Month 6+):** 312K routing decisions from 600 LTD users — which agent solves which task type fastest and cheapest. Unforkable.
- **Switching cost (Day 1):** Config files, cost history, workflow definitions, Telegram bot connection — user-specific state that accumulates from first use.
- **Ecosystem lock-in (Month 9+):** Linear OAuth app, Plane marketplace listing, JetBrains plugin approval, agent vendor relationships — legal agreements a fork can't clone.
- **Brand & benchmark (Month 18+):** "SPUR Agent Performance Report" becomes the J.D. Power of AI coding agents. Measurement systems shape markets.

**The PicoClaw lesson:** OpenClaw was forked 12+ times in 6 weeks. ZeroClaw, Nanobot, PicoClaw, IronClaw — all technically superior in at least one dimension. None displaced OpenClaw. Community, brand, and ecosystem gravity > code.

---

## 8. Distribution Strategy

**Build gravity, not pitch decks.**

**Month 1-6: Direct to developer (B2C)**
- Open source on GitHub → organic discovery
- HN launch: "I built a Rust CLI that saved me $67/week routing across AI agents"
- Vietnamese developer communities — Kevin's unfair advantage, zero competition
- Claude Code / Gemini CLI plugin ecosystem — ride existing distribution

**Month 4-9: Partner channels (B2B2C)**
- Managed hosting providers (OpenClawVPS model) — wholesale at $10/instance/month
- Educator affiliates — 20% commission, "free AI tools for students"
- PM tool marketplaces — Linear integrations directory, Plane marketplace

**Month 12+: Agent vendor gravity (earned, not pitched)**
- Contribute ACP bug fixes upstream to agent repos
- Publish Agent Performance Reports — vendors cite our data
- They approach us (not the other way around)

---

## 9. Technical Highlights

**Single binary. Zero dependencies. Protocol-native.**

- **Language:** Rust (memory-safe, fast, single binary compilation)
- **Binary size:** <15 MB target
- **Memory:** <30 MB idle, <100 MB with 5 active sessions
- **Routing overhead:** <1ms (rule-based), <2s (brain-assisted via Gemini free tier)
- **Protocol:** ACP (Agent Client Protocol) — JSON-RPC 2.0 over stdio
- **Storage:** SQLite for cost tracking and routing history (local, portable)
- **Distribution:** `cargo install spur-cli` or `curl | sh` (via cargo-dist)
- **Platforms:** Linux (x86/ARM), macOS (Intel/Apple Silicon), Windows
- **License:** Apache 2.0 (core), Commercial (enterprise features)

**Key Rust crates:** ratatui (TUI), tokio (async), serde_json (ACP), reqwest (APIs), rusqlite (storage), teloxide (Telegram), clap (CLI)

---

## 10. Roadmap

| Phase | Timeline | Deliverable | Milestone |
|---|---|---|---|
| **v0.1** | Week 1-3 | ACP client + Gemini/Kiro + cost tracking | First `spur run` works |
| **v0.2** | Week 3-7 | TUI dashboard + Claude wrapper + failover | "Wow" moment: auto-route on rate limit |
| **v0.3** | Week 7-12 | Workflow engine + scoring + LTD launch | 600 LTD users, $47K cash |
| **v0.4** | Month 4-6 | Telegram gateway + Linear integration | "Issue in, PR out" demo |
| **v1.0** | Month 9-12 | Team dashboard + smart routing | Team tier launch, $4.8K MRR |
| **v2.0** | Month 18+ | Agent Performance Report + enterprise | $100K ARR, industry benchmark |

---

## 11. The Ask

**SPUR is building the coordination layer that doesn't exist yet.**

- Nobody owns the space between PM tools and AI coding agents
- 30+ ACP agents are waiting to be orchestrated — no partnership required
- Zero infrastructure cost per user — binary runs locally
- Data moat starts accumulating from Day 1 of user activity
- The "Kubernetes of AI coding agents" opportunity is open

**Get started:**
- Web: spur.wtf
- Docs: spurengine.dev
- GitHub: github.com/spurengine/spur
- Contact: kevin@spurengine.dev

---

*SPUR — drive your agents into coordinated action.*
