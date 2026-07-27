# marketing/ (not part of the OSS product tree)

Go-to-market artifacts (campaign plans, competitor research, Product Hunt
ops, site copy) are **out of scope** for the public SPUR monorepo.

They remain useful for company ops and may still exist on disk locally, but
they are **gitignored** here so clones stay engineering-focused.

## Where this content should live

| Content | Intended home |
|---|---|
| Campaign plans, VOC, competitors, site/PH copy | Private `getspur/spur-marketing` (or equivalent) |
| Launch video + stills | `getspur/spur-media` / CDN (see `videos/`, `deliveries/`) |

## Local layout (optional, untracked)

If you keep a local marketing tree for agent skills, typical layout:

```text
marketing/
├── product-marketing.md
├── CAMPAIGN_PLAN.md
├── competitors/
├── launch/
├── messaging/
├── research/
├── site/
└── marketingskills/   # vendored skill packs; do not commit secrets
```

Do not re-add marketing deliverables to this repository without an explicit
decision to expand OSS scope.
