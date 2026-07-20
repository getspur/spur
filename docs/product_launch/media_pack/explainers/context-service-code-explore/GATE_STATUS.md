# Gate status — context-service-code-explore

| Gate | Status | Date |
|---|---|---|
| concept_layout | **approved** (Direction A) | 2026-07-20 |
| script_storyboard | **approved** | 2026-07-20 |
| paid_generation | pending | — |

## HTML plates rendered (exact duration, silent, Direction A)

| Asset | Path | Duration | Spec |
|---|---|---|---|
| plate-edges-dark | `plates/out/plate-edges-dark.mp4` | 11.0s | 1920×1080 H.264 @24fps |
| plate-two-planes | `plates/out/plate-two-planes.mp4` | 13.0s | 1920×1080 H.264 @24fps |
| plate-selector | `plates/out/plate-selector.mp4` | 8.0s | 1920×1080 H.264 @24fps |
| plate-tool-loop | `plates/out/plate-tool-loop.mp4` | 14.0s | 1920×1080 H.264 @24fps |

Posters: `plates/out/*-poster.jpg`

Source HTML: `plates/html/*.html`  
Renderer: `plates/render-plates.mjs` (canvas `__draw(t)` → JPEG pipe → ffmpeg)

**Note:** html-video app MCP was unavailable (flush/trust); plates produced via deterministic canvas pipeline with the same self-contained contract (exact duration, no baked VO).

## Timeline map (visual)

| t | Scene | Asset |
|---|---|---|
| 0–4 | S01 open | Palmier native title |
| 4–15 | S02 | plate-edges-dark |
| 15–28 | S03 | plate-two-planes |
| 28–36 | S04 | plate-selector |
| 36–50 | S05 | plate-tool-loop |
| 50–60 | S06 CTA | Palmier native end card |

## Gate 3 proposal — paid generation (awaiting approval)

### Creative shots
**None.** Atmospheric B-roll declined. VO only.

### Model / voice
| | |
|---|---|
| **Job type** | `inworld_text_to_speech` |
| **Voice** | `Graham (en)` — clear, operator-grade English |
| **Shot count** | 0 video · **6 audio segments** (scene-aligned) |
| **Aspect** | n/a (audio) |
| **Retry ceiling** | 2 per segment; rejoin timeouts; no duplicate jobs |
| **Account** | plus plan · ~2401 credits (sample cost: 2 credits / short line) |

### Tightened VO (fits ~60s · ~130 words · claims-only)

| Seg | Budget | Script |
|---|---|---|
| **N01** | 0–4s | SPUR Context Service is cloud code context for third-party packages. |
| **N02** | 4–15s | Inside your repo, call sites resolve. Cross a dependency, and the edge goes dark — the agent guesses. Without structured external context, teams fall back to web search, outdated docs, or wrong APIs. That breaks integrations and burns rework. |
| **N03** | 15–28s | Two planes. Worktree tools — knowledge context pack and code tools — answer what's in this repo. External MCP tools from Context Service answer what's in dependency X at version Y. |
| **N04** | 28–36s | Every unit is source, package, and revision. Selectors pin the version — for example, serde one-point-oh-one-nine-seven, trait Deserialize. No stale docs. No version drift. |
| **N05** | 36–50s | Same graph-first loop outside the worktree: orient, read real source, walk callers and callees. If a revision is cold, index on demand, keep working, poll status. Complements local code tools — never mix package selectors into worktree reads. |
| **N06** | 50–60s | Version-precise answers without cloning every crate. Open context dot get spur dot dev. |

**Not generating:** music, captions, product UI footage, logos, title cards (Palmier owns titles).

### Reply to unlock generation
`approve Gate 3` (or change voice / keep long script / drop segments)

## Gate 3 — APPROVED & executed

- Voice: Graham (en) via `inworld_text_to_speech`
- VO tightened for scene fit (v2 scripts under `vo/v2/`)
- Master: `delivery/context-service-code-explore-v1.mp4` (~61.3s H.264/AAC 1920×1080)
- Assembly: ffmpeg composite (PalmierPro MCP not available in session); titles as native design stills
- Manifest: `delivery/MANIFEST.json`

### Space note
Regenerable `dist/` release binaries were cleared mid-session to free disk for render.
