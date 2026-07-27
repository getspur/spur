# OSS boundary

This monorepo is the **public SPUR product** (CLI, orchestration, TUI, MCP,
build tooling). Company marketing, production media, and operator scratch are
kept out of the default tree so clones stay small and contributor-focused.
Small, web-optimized product demos may live under `docs/demos/` when they are
referenced directly by public documentation.

## In scope (tracked)

| Path | Role |
|---|---|
| `crates/` | Product source |
| `scripts/`, `xtask/`, `npm/`, `tests/`, `third_party/` | Build, release, quality |
| `docs/architecture*`, `docs/user-docs/`, `docs/onboarding/`, design/RCA as needed | Engineering docs |
| `docs/demos/` | Curated, web-optimized product demos used by public docs |
| `.github/` | CI / release |
| `.spur/config.toml.example`, `.spur/skills/` | Example config + product skills |
| Root: `README`, `LICENSE`, `CONTRIBUTING`, `CHANGELOG`, `ARCHITECTURE`, Cargo files | OSS hygiene |

## Out of scope (gitignored; pointer READMEs only)

| Path | Why |
|---|---|
| `marketing/**` | GTM, competitors, campaign plans |
| `videos/**`, `deliveries/**`, `video-assets/` | Raw recordings, films, and production outputs |
| `docs/product_launch/**` | PH packs, media, launch checklists |
| `output/` | One-off migration dumps / screenshots |
| `app_gallery/`, `sdk/` | Local checkouts, not monorepo product |
| `.spur/config.toml`, `.spur/s3-*`, `.spur/s4-*`, logs | Operator/machine state |

## Planned sibling homes

| Content | Suggested repo |
|---|---|
| Marketing copy & research | `getspur/spur-marketing` (private) |
| Launch / explainer media | `getspur/spur-media` (+ LFS or object storage) |
| Hosted context-service ops | Optional private ops repo for `infra/` (future) |

## History note

**2026-07-27:** `main` was rewritten with `git filter-repo` to drop historical
blobs under:

- `docs/product_launch/`, `videos/`, `deliveries/`, `marketing/`, `output/`
- `crates/spur-notebook/` (moved out of monorepo earlier)
- `.spur/analyst-overlays/`, historical `vendor/`
- residual `*.mp4` / `*.mp3` / `*.m4a` / `*.wav` / `*.parquet`

Recovery point (pre-rewrite tip + media): tag
`backup/pre-oss-history-purge-20260727T010021Z` → commit `536fb6cf9`.

**Everyone with an existing clone must re-sync** (rewritten history):

```sh
git fetch origin
git checkout main
git reset --hard origin/main
# or re-clone fresh
```

Local feature branches / worktrees based on pre-purge SHAs will not merge
cleanly; recreate them from the new `main` or cherry-pick carefully.

**Local 14GB `.git`:** often packed local unreachable objects and worktrees,
not origin size. After reset: `git reflog expire --expire=now --all && git gc --prune=now`,
or re-clone.

## Do not re-add

- Unoptimized production media; keep public `docs/demos/` assets curated and
  under 10 MB each
- Parquet analyst overlays
- Local IDE skill forests for third-party media tools as product defaults
- Operator `terraform.tfvars` or env secrets

See also: root `.gitignore` section “OSS boundary”.
