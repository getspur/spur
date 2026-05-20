# HN Submission Plan — Launch Blog Post

*Companion to `hn-blog-post.md`. Pre-flight for the Hacker News submission. No "Show HN:" prefix — there's no public repo to show.*

---

## (a) Submission title

> **I was running 5-10 Claude Code agents in tmux. Then I hit the weekly cap on day three.**

- 76 characters. Under HN's 80-char ceiling.
- Story-shaped. Reads as a personal post, not an announcement. Doesn't contain "Introducing," "Launch," "Announcing," "We built," or a product name.
- Both halves are direct VOC pulls: the "5-10 agents in tmux" half from Beefin (HN 47104424), the "hit my weekly in 3 days" half from esperent (HN 47626833). Two HN readers will recognize their own thread on sight.
- Title matches the blog post H1 exactly — required by HN guidelines.

**Backup titles** (in case the lead doesn't fly within first 30 minutes and we need to resubmit, per HN's same-URL second-chance pool):

1. *"What broke when I started running five CLI coding agents at once"* — 64 chars, less specific hook, removes the Claude reference.
2. *"A control tower for the CLI coding agents I was juggling in tmux"* — 65 chars, lifts Beefin's verbatim phrase as anchor.

Do **not** retitle to: "Show HN: SPUR" (no repo), "SPUR — control tower for AI agents" (product-name-first reads as an ad).

## (b) Submission URL

```
https://getspur.dev/blog/control-tower
```

Submit the blog post URL, not `getspur.dev` and not `getspur.dev/demo`. Rationale:

- HN penalizes thin domain-root submissions as advertising. A blog URL with the same title as the post reads as a story.
- The blog post itself contains the install CTA, the demo link, and the `/vs/*` links — readers reach the product without us needing the submission URL to be the product page.
- The blog post is on `getspur.dev` rather than a personal Substack or Medium because (i) the founder voice in the post still ties to the product clearly, (ii) personal blogs without prior HN history tend to get flagged as marketing-in-disguise just as fast as company blogs do, and (iii) the canonical URL we want indexed is on our own domain. See part (c) of the summary below for the longer recommendation.

## (c) Self-comment seed (≤30 words)

Post within the first 90 seconds of submission, before any external comments. Pin nothing — HN doesn't pin.

> Author here. SPUR is proprietary (no repo to Show HN), in active beta, and the post deliberately lists what it doesn't do before the price. Happy to take the rough questions.

Word count: 30. Establishes (i) it's me, (ii) why no Show HN prefix, (iii) maturity stage, (iv) invites adversarial questions rather than ducking them.

---

## (d) Six likely top-voted critical comments — prepared honest responses

Each anticipated comment is paraphrased from real patterns observed on HN threads about Claude Code, multi-agent tooling, and developer-tool launches. Responses are written to be read by the HN audience, not to "win" — defensive answers are how launches die in this venue.

### 1. "Why isn't this open source? Hard pass."

**Prepared response:**

> Fair. The orchestration core is closed because the cost extractors + license server + Telegram bot are the only durable moat I have, and shipping them as OSS day one means losing the company before it has revenue. Community tier is free and runs without a key under the EULA — same review loop, same cost ledger, same lineage as Pro. We may open-source select crates over time (telemetry, ACP client) but I'm not going to commit to a date I can't keep. If "proprietary" is a deal-breaker, Aider and Amux are both excellent and OSS — I link to both in our competitor docs and I mean it.

### 2. "This is just tmux + worktrees with extra steps."

**Prepared response:**

> Honestly, yes — for one or two agents. The post says that out loud. The wedge is at fleet size ≥ 2, and the three things tmux can't give you are (a) a plan that survives `kill -9 $$` and laptop close, (b) a review state machine with timeout / retry / cherry-pick, (c) a cost ledger that sums across five vendor CLIs. If your fleet is one agent, install Aider; SPUR is wasted bytes. If your fleet is five and you're keeping the plan in a `~/scratch/plan.md`, the math eventually flips. I'd rather you stay in tmux than churn off Pro in month two.

### 3. "Cross-vendor failover is a parlor trick. Codex doesn't have Claude's context window state."

**Prepared response:**

> The handoff is task-level, not in-flight conversation-level — to be precise: when we swap, Codex gets the worktree state, the task spec, and any prior worker's attempt as context, but it does not get Claude's hidden chain-of-thought. The honest framing is "the plan continues, the conversation restarts on the new vendor." The demo shows Codex `apply_patch`ing the same file Claude was reading; what it does not show is Codex re-deriving the bit of reasoning Claude had partially formed. For atomic tasks that's fine; for a single 200k-token research thread it's a clear regression vs. waiting for Claude's window to reset. The product surfaces both choices.

### 4. "Lifetime SKU on a beta tool with no funding disclosed = pre-bankruptcy fire sale."

**Prepared response:**

> Reasonable read and I want to address it directly. The `$290` lifetime maps to a `personal_lifetime` plan key already in the license crate — it predates the launch decision. We are not running a countdown, not capping it at "first 100 buyers," and the post says so. If we ever retire it we retire it for new buyers only and honor every existing license. If you'd rather wait and pay monthly until we've proven we're around in a year, that is the rational choice and I'd make the same call. Monthly is $19. The lifetime exists for the third bucket of buyer who prefers a one-time payment to a recurring one — not as a survival signal.

### 5. "Where's the security model? You're reading every vendor's JSONL on my disk."

**Prepared response:**

> Good question, here's the architecture: SPUR runs locally as a single Rust binary, reads vendor JSONL/SQLite from the paths the vendor CLIs already write to on your machine, and never proxies your traffic. There is no SPUR cloud component for the cost ledger. The only network traffic is (a) license validation against our dashboard if you're on a paid tier (Ed25519-signed policy doc, refreshable offline), (b) Telegram if you opt in, (c) opt-in Tier-2 telemetry (`SPUR_TELEMETRY=1`, off by default). The orchestration core, the plan store (beads SQLite), and the event log (NDJSON) are all local-disk. Source for the crates we plan to open is on the roadmap.

### 6. "I'd love to like this but the post reads like a launch post pretending not to be a launch post."

**Prepared response:**

> That's a fair critique and I'll own it — there's no clever workaround for "founder posts about own product on HN." What I can say is that the post lists six things SPUR doesn't do before it lists the price, the pricing claim is anchored on a license-key constant that already exists in the code rather than a launch promo, and the cost-discrepancy framing cites two HN comments by username and item ID so anyone can spot-check. None of that makes it not a launch post. It just means I'd rather be transparent that I'm launching than pretend I'm not.

---

## Summary

### (a) Paragraph most likely to be quoted in a top HN comment

The buremba-anchored paragraph in section "What 'the top half' turned out to be," specifically the line **"A 5× gap between what you think you're spending and what you're actually spending is a problem you can solve only if you can see it."** Both favorable and hostile commenters will quote it: favorable readers will use it to validate the cost-ledger thesis; hostile readers will quote it to set up "and your product doesn't actually solve that, it surfaces it." Either way the line is the load-bearing claim of the post and it does the work of summarizing the cost-opacity theme in one sentence — so it is also the line we want excerpted.

### (b) Strongest "this won't fly on HN" risk

The **closed-source disclosure plus the lifetime SKU on a beta tool**, appearing in close proximity in the post, is the single highest-risk combination. Each is defensible on its own; together they read to a skeptical commenter as "proprietary tool, pay us forever-money up front, trust us." The post mitigates this by (i) addressing the proprietary question with a direct "here's why" rather than a deflection, (ii) anchoring the lifetime SKU to a pre-existing license-crate constant rather than a launch event, and (iii) explicitly telling the reader "wait and pay monthly if you'd rather see us survive a year first" — but the structural risk remains. The mitigation in the comment thread, if this becomes the dominant critique, is to surface response #4 within the first 10 minutes of the thread (proactively, not just when asked) so the prepared answer is in the early-cache that drives downvote/upvote velocity. If the critique still dominates the top three comments at the 30-minute mark, the next move is to publish the license-crate code excerpt as a follow-up comment with the file path and SHA — concrete artifact beats prose every time.

### (c) Personal blog or company blog — recommendation

**Recommendation: company blog (`getspur.dev/blog/control-tower`), not a personal blog.**

Reasons:

1. **HN treats both as marketing anyway.** The argument for a personal blog is "more authentic." In practice, an audience that has read 10,000 HN launch threads detects authorial intent in the first paragraph regardless of which domain hosts it. The penalty for a thinly-disguised company-launch-on-personal-blog is *higher* than for a direct company-blog post that owns its own framing. The post is in the founder's voice; that does the authenticity work the domain choice can't.
2. **Canonical URL goes where the SEO equity should accrue.** If the post lands on HN's front page, the inbound links are worth meaningful domain rating to `getspur.dev`. A personal blog would orphan that equity.
3. **The `/vs/*` links and the install CTA live on `getspur.dev`.** Hosting the post on the same domain lets readers move from post → demo → comparison page → install without a domain-hop, which kills bounce velocity.
4. **A future second HN post from a personal blog re-uses the same "personal post" trick once.** Burning it on the launch is wasteful — save the personal-blog play for a future post where the founder has a story that isn't directly about the product (e.g. "what I learned shipping a Rust TUI to 5,000 installs").

Caveat: this recommendation assumes the founder has no prior HN-resident personal blog with established karma history. If there is an existing personal domain that HN already trusts (multiple front-page posts in the last 12 months), the calculus flips — at that point the trust signal of the established personal domain outweighs the canonical-URL argument and the post should go there with a cross-link from `getspur.dev/blog`. Verify before committing.
