# SPUR Product Hunt video review

**Reviewed:** 2026-07-16
**Decision:** Ship the multi-source product hero for Product Hunt. Keep generated trailer material in social channels only.

## Approved product video

| Field | Result |
|---|---|
| File | `../../ph_ready/hero-video-ph-ready.mp4` |
| Origin | Five fresh SPUR TUI captures bound by `../../proof-manifest.json` |
| Duration | 25.2 seconds |
| Resolution | 1920 by 1080 |
| Aspect | 16:9 |
| Codec | H.264 |
| Audio | None |
| Opening | Three-second SPUR title frame |
| Close | Three-second install frame |
| Captions | Burned-in, claim-bound labels for all five proof segments |

### Story and evidence

| Beat | Evidence | Review |
|---|---|---|
| Hook | Control tower for CLI coding agents. | Clear audience and role in the first frame. |
| Operator home | Session Detail with INSERT and following. | Proves the working surface without invented UI. |
| Visibility | WORKERS and worker state in Go to. | Proves state visibility. It does not claim active delegation. |
| Plan state | Plans with No plans found. | Honest observe-only state. It does not claim campaign progress. |
| Routing | agent, model, and effort visible before dispatch. | Proves explicit specialist controls. |
| Durability | Resumed from prior conversation. | Proves session return in the captured flow. |
| Resolution | Install command. | Clear next action. |

The former single-source edit was rejected as an evidence model because it assigned worker, plan, specialist, and resume claims to one plan-loop film. The approved render cuts each claim from its own reviewed source and preserves those segment IDs through the final encode.

## Generated marketing trailer

Content under `../out/`, including `06-trailer.mp4` when present, is generated marketing treatment. It may be used for launch posts, paid social, or a decorative site loop. It must not be used as the Product Hunt product video or as a gallery screenshot because intermediate frames can invent UI details.

| Channel | Approved asset |
|---|---|
| Product Hunt video | Real multi-source hero only |
| Product Hunt gallery | Five real proof stills only |
| Product Hunt thumbnail | SPUR thumbnail from `ph_ready/` |
| X or LinkedIn | Generated trailer may accompany a real still and product link |
| Website decoration | Generated loop allowed when labeled and separated from proof |

## Verification record

The repository contract checks source checksums and timestamps, gallery crop bounds and dimensions, OCR proof terms, hero graph identity, caption bindings, codec, dimensions, duration, and segment retention. Rebuild and verify with:

```bash
docs/product_launch/media_pack/refresh.sh
docs/product_launch/media_pack/demo_render/build.sh
bash docs/product_launch/media_pack/tests/media-contract.test.sh
```

If any source capture changes, update `proof-manifest.json` only after reviewing the new frame and claim. A fresh checksum alone is not approval.
