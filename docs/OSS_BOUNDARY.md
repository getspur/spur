# OSS boundary

This monorepo is the **public SPUR product** (CLI, orchestration, TUI, MCP,
build tooling). Company marketing, launch media, and operator scratch are
kept out of the default tree so clones stay small and contributor-focused.

## In scope (tracked)

| Path | Role |
|---|---|
| `crates/` | Product source |
| `scripts/`, `xtask/`, `npm/`, `tests/`, `third_party/` | Build, release, quality |
| `docs/architecture*`, `docs/user-docs/`, `docs/onboarding/`, design/RCA as needed | Engineering docs |
| `.github/` | CI / release |
| `.spur/config.toml.example`, `.spur/skills/` | Example config + product skills |
| Root: `README`, `LICENSE`, `CONTRIBUTING`, `CHANGELOG`, `ARCHITECTURE`, Cargo files | OSS hygiene |

## Out of scope (gitignored; pointer READMEs only)

| Path | Why |
|---|---|
| `marketing/**` | GTM, competitors, campaign plans |
| `videos/**`, `deliveries/**`, `video-assets/` | Films and production outputs |
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

Removing paths from the current tree does **not** purge historical blobs.
A later `git filter-repo` / BFG pass may shrink clone size; that requires a
coordinated force-push and is intentionally **not** part of the first cleanup.

## Do not re-add

- Multi‑MB `.mp4` / `.mp3` / parquet analyst overlays
- Local IDE skill forests for third-party media tools as product defaults
- Operator `terraform.tfvars` or env secrets

See also: root `.gitignore` section “OSS boundary”.
