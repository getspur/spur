# marketing/

All marketing artifacts for SPUR live here, isolated from the engineering codebase.

## Layout

```
marketing/
├── README.md                    ← this file
├── product-marketing.md         ← V1 foundation (also symlinked to .agents/product-marketing.md)
├── CAMPAIGN_PLAN.md             ← campaign-as-beads-plan sketch
├── marketingskills/             ← cloned from coreyhaines31/marketingskills (gitignored)
│                                  (symlinked into .claude/skills/ as marketing-*)
│                                  Reinstall: `git clone --depth=1 \
│                                    https://github.com/coreyhaines31/marketingskills.git \
│                                    marketing/marketingskills`
├── artifacts/                   ← outputs from skill runs
├── research/                    ← customer research, VOC, transcripts
├── competitors/                 ← competitor profiles (one .md per competitor)
├── messaging/                   ← positioning, value props, psychology levers
├── site/                        ← landing page / pricing / VS pages copy + assets
├── launch/                      ← Product Hunt / HN / press kit
├── seo/                         ← audits, AI-citation strategy
├── content/                     ← blog posts, pSEO templates, lead magnets
├── outbound/                    ← cold email, ad strategy
├── ads/creative/                ← ad copy variants
├── partners/                    ← co-marketing shortlists
├── social/                      ← post calendar
├── community/                   ← Discord/referral plans
└── measurement/                 ← tracking plan, A/B backlog, lifecycle
```

## Conventions

- **Foundation first.** Every skill run reads `product-marketing.md` before producing anything.
- **Beads ID prefix.** All marketing issues use `mkt.*` so they don't collide with engineering work.
- **One artifact per task.** Keeps reviewer load sane and parallelism clean.
- **No marketing artifacts in the repo root.** Anything not here is a bug.

## How to invoke a skill

```
/marketing-{name}
```

e.g. `/marketing-cro` for conversion review, `/marketing-cold-email` for outbound sequences. The skill will read `product-marketing.md` automatically.

## How to dispatch through Spur

See `CAMPAIGN_PLAN.md` § "Dispatch Pattern".
