# I was running 5-10 Claude Code agents in tmux. Then I hit the weekly cap on day three.

*Draft for HN submission. Posted on the SPUR blog at `getspur.dev/blog/control-tower`. Author: a founder. First-person, no logos, no "introducing." If you came from Hacker News, the comment thread is probably more useful than this page; scroll if you want the install command — it's a curl.*

---

## The afternoon I lost an hour

I had Claude Code mid-`apply_patch` on a file I'd been staring at for ten minutes. Read tool, write tool, the usual rhythm. I hit Enter on the next prompt.

```
Claude usage limit reached. Your limit will reset at 3pm.
/upgrade to increase your usage limit.
```

It was 11:14 a.m. on a Wednesday. I was on the Max plan. I had not done anything unusual — no megacontext, no 1M tokens, no overnight Reflexion loop. I had just been working.

I closed the tab. Opened Codex in the next pane. Started re-explaining the file, the constraint, the half-written diff Claude had left in the worktree. By the time I'd primed Codex on enough context for it to be useful, it was 11:43. I had spent twenty-nine minutes putting an agent back where another agent had been thirty seconds earlier.

That's the moment. If you've subscribed to one of these things, you've had this moment. The corpus of Hacker News comments on Claude Code's weekly limits reads like a group therapy session for it:

> *"Paying $200 a month, I hit my weekly in 3 days last week."* — esperent, HN 47626833

> *"You're locked out of the service … while still paying your subscription … It's ridiculous."* — TheOtherHobbes, HN 44713757

The thing that bothered me wasn't the rate limit. The rate limit is a pricing decision and I don't have strong opinions about Anthropic's pricing. The thing that bothered me was that I had no continuity. Nothing in my tooling treated "the plan I was working on" as something that survives the agent serving it. I was the continuity. My short-term memory was the queue. My tmux tab title was the state machine. That worked when I was running one agent. It stopped working when I was running five.

## The shape of what I had been doing

If you run two or more CLI coding agents on a real codebase, you have probably built some version of this stack yourself:

- A `git worktree add` per parallel task, named by something you'll forget in ten minutes.
- A tmux session per worktree, because Claude Code wants its own terminal and so does Codex and so does whatever else.
- A scratch file — maybe in a Notion page, maybe in `~/scratch/plan.md` — where you keep what each agent is doing, because the tmux tab title cuts off at the seventh character.
- An ambient sense, hopefully accurate, of which agents are "stuck," which are "working," and which are "done but I haven't reviewed yet."
- An even more ambient sense of how much money is currently being burned.
- A small private prayer that none of the agents are stomping each other's files.

On HN, nojs put it cleanly:

> *"Yes, worktrees with workmux. I expected this to become less necessary over time as models got faster, but the opposite has happened."* — nojs, HN 47573483

That matched my experience. Faster models did not reduce the coordination tax. They increased the number of agents I was willing to run in parallel, which increased every problem above. Beefin, who wrote a tool called Amux for the same itch, said the part out loud:

> *"I run 5-10 Claude Code agents at a time across different repos. Keeping track of which one is waiting for input, which one is working, and which one broke something was chaos. I needed a control tower."* — Beefin, HN 47104424

I needed a control tower too. I'd built the bottom half of one in tmux. The top half didn't exist.

## What "the top half" turned out to be

I started writing SPUR in Rust because I wanted four things tmux couldn't give me, and that I was tired of approximating with shell scripts:

**A durable plan.** Not a tab title. A DAG of subtasks, persisted in SQLite (we use a tracker called beads), with each node owning its own worktree path, its own assigned worker, and its own review state. Close the laptop, restart SPUR, the plan is still there.

**A review queue that is a state machine.** Not a chat. Approve / Reject / Modify / Retry, with timeout and merge gating. The review gate is the load-bearing primitive — it decides what gets cherry-picked onto staging and what gets thrown away. I want this to be a state machine because state machines survive crashes and chat windows do not.

**A cost ledger that spans every vendor I pay.** Claude bills me. Codex bills me. Gemini bills me. Kimi bills me. OpenCode bills me. None of them tell me the sum, because none of them know about the others. SPUR reads the JSONL or SQLite each vendor writes to disk in place — no proxy, no second account — and shows me one number. buremba's quote is the one that made me commit to building this part:

> *"I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!"* — buremba, HN 44598254

A 5× gap between what you think you're spending and what you're actually spending is a problem you can solve only if you can see it. None of the per-vendor CLIs are going to show you their competitors' numbers. The ledger has to live in something neutral.

**A way to swap brains mid-flow without losing context.** This is the one I cared about most after that Wednesday morning. If Claude is rate-limited, the plan should not be rate-limited. The plan should pick up on Codex from the same task node, same worktree, same files. When Claude's window resets, swap back. The agent is interchangeable; the plan is not.

That last one is the part you cannot do inside any single-vendor tool, by definition. It's also the part that's hardest to describe in writing, so we shot a 90-second screen capture: Claude prints the red rate-limit message, the SPUR Plan Inspector opens over the dead window, Command Palette → `switch worker` → `codex`, the worker badge flips, and Codex starts `apply_patch`ing the same file Claude was reading 25 seconds earlier. No edits, no mocks. (Linked at the bottom.)

The whole loop ends up looking like this:

```
   ┌─────────────┐      ┌──────────────────────┐      ┌─────────────────┐
   │  Issue in   │ ───▶ │ Workers in parallel  │ ───▶ │ Review surface  │
   │             │      │                      │      │                 │
   │ submit_plan │      │  Claude · Codex      │      │ Approve / Reject│
   │ (beads SQL) │      │  Gemini · Kimi       │      │ Modify / Retry  │
   │             │      │  OpenCode            │      │ Cherry-pick DAG │
   │             │      │  one worktree each   │      │ → staging branch│
   └─────────────┘      └──────────────────────┘      └─────────────────┘
```

A brain agent reads the task and emits a DAG. Each subtask runs in its own worktree under `spur/worker/v2/{agent}/...`. Approved diffs cherry-pick onto a staging branch in DAG order. The same state machine is reachable from a Telegram bot, because the day I built the review queue I was sitting in an airport and wanted to merge the night's work from my phone.

That's it. That's the product. It is not magic. It is a kernel for agent execution that happens to wear a TUI.

## What this post is not promising

A thing I noticed reading the indirect-competitor profiles in our own repo is how much marketing copy in this space is written like a bumper sticker. "Autonomous agents for any task." "AI-native platform." "Revolutionary." I am going to do the opposite — write what SPUR doesn't do, and write it before the price.

- **There is no autonomous mode.** Every worker output passes through a human review gate. If you want "assign tickets in Slack, get PRs back, never look at a diff," that's Devin. Devin is good at that. SPUR keeps the human in the loop on purpose.
- **There is no SOC 2 or HIPAA at launch.** Both are on the Enterprise roadmap. If procurement requires either, we are not ready for you.
- **There is no per-developer budget cap enforcement.** We surface spend per developer, per repo, per week. We do not gate it. If you need an enforced ceiling, this is an Enterprise concern we have not built.
- **There is no public source repository.** SPUR is proprietary. The binary is signed and distributed via `curl | sh` from getspur.dev. We may open-source select crates over time (telemetry, the ACP client). The orchestration core stays closed. This is also why this post is on our blog and not a Show HN — there is no repo to show.
- **There is no on-prem or air-gapped deploy at launch.** Cosine Genie owns that segment. We have no story there.
- **There is no PTY scraping.** SPUR speaks native ACP + MCP. If your agent does not speak ACP, it is not a worker today. Most modern CLIs do.

If any of those is a deal-breaker, you should know now and not after the install.

## The wedge: you already built half of this

If you are reading this on HN, you are probably in one of three buckets:

1. You run one CLI agent and you're happy. SPUR is not for you yet. Aider and Claude Code are excellent at being one CLI agent. Adding SPUR above one agent is a waste of a UI.
2. You run two or more CLI agents and you live in tmux. You have already built the worktree-per-task pattern. You have a scratch file. You have the muscle memory. SPUR is the layer above what you've already built. Keep your tmux. Keep your worktrees. We add the durable plan, the review queue, and the cross-vendor ledger.
3. You want a fully autonomous engineer in Slack. Hire Devin. Genuinely.

If you're in bucket 2, you are the user. The pitch is not "switch from your stack." It's "you already wrote the bottom half — let us finish the top half."

## Pricing, in one paragraph

Community is $0, no key, no signup, no credit card — one brain, one worker, the full review loop, the full cost ledger, full event-sourced lineage. Pro is **$19 / seat / month** (or $182 / year, or **$290 one-time, lifetime**). Pro unlocks parallel workers, session resume via event replay, brain-swap across vendors, DAG cherry-pick, Reflexion retry, and the Telegram bot. Team is $49 / seat / month with a three-seat minimum. Enterprise is contact-sales, roughly $25k/yr floor.

A note on the lifetime SKU because I expect a question about it: it is not a launch promotion. It maps to a `personal_lifetime` plan key that already ships in our license crate. We are not running a countdown on it. We are not capping it at "the first 100 founding seats." It exists because the code supports it, and if we ever retire it we will retire it for new buyers and honor every existing license. "Lifetime" means lifetime.

## The install, the demo, the comparisons

If you want to try it, the install is one line:

```
curl -sSL https://getspur.dev/install.sh | sh
```

That installs a signed Rust binary. No Node, no Python, no Docker. Run `spur init` and it auto-detects whichever of Claude Code, Codex, Gemini, Kimi, or OpenCode you already have installed.

The 90-second screen capture I described above is at `getspur.dev/demo`. If you have hit a rate limit in the last 30 days, the first 15 seconds will be familiar.

For the inevitable "how is this different from X" question, three comparison pages live at `getspur.dev/vs/claude-code`, `getspur.dev/vs/cursor`, and `getspur.dev/vs/devin`. The short version of all three: we are not competing with any of them. Claude Code is one of our workers. Cursor lives in another window. Devin owns a different job. We are the layer that sits across the CLI agents you already pay for and gives them one plan, one review surface, and one ledger.

If you read this far and your reaction is "this is a feature, not a product," I would gently disagree, but I would also rather you tell me on HN than not tell me at all. The thread is open. I'll be in it.
