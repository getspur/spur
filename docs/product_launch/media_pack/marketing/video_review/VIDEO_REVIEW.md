# Video review — SPUR Product Hunt pack

**Date:** 2026-07-16  
**Reviewer lens:** marketing-video skill (demo vs social, platform specs, captions, authenticity)  
**Product truth:** `SPUR_PRD.md` v2.3 · `product-journey-ph.md` · real VHS from `tui-live/out`  
**Virality Predictor (`brain_activity`):** unavailable in current `higgsfield generate create` (returns *Model type "text" is not supported*) — scores below are editorial, not brain_activity API.

---

## Videos under review

| ID | Path | Origin | Intended channel |
|---|---|---|---|
| **V1 Hero (product)** | `media_pack/ph_ready/hero-video-plan-loop-drive.mp4` | Real VHS `13-problem-plan-loop-drive` | **Product Hunt video** · product page embed |
| **V2 Trailer (marketing)** | `media_pack/marketing/out/06-trailer.mp4` | Seedance 2.0, conditioned on real stills | Social only (X/LinkedIn) — **not** PH product gallery video |

---

## Technical scorecard

| Spec | V1 Hero (VHS) | V2 Trailer (Seedance) | Notes |
|---|---|---|---|
| Duration | **48.8 s** | **8.1 s** | PH tolerates ~60–90s demos; social wants ≤15s hooks |
| Resolution | **2560×1600** | **1280×720** | Hero is high-res capture; trailer is 720p marketing |
| Aspect | ~16:10 (native TUI) | **16:9** | PH OK; crop hero to 16:9 for YouTube if needed |
| Codec | H.264 | H.264 + **AAC stereo** | Trailer has audio; hero is **video-only** |
| Bitrate | ~0.25 Mbps | ~9.9 Mbps | Hero is sparse terminal (fine); trailer is dense |
| Frame content | Real Session Detail / plan loop | AI motion on real stills | Product truth vs stylized motion |

**Keyframe density (luma variance proxy):** hero mid/late frames show real UI content (std ~ higher mid-film); t=2s nearly empty (startup). Trailer frames denser throughout (motion + grading).

---

## V1 — Hero product film (plan-loop-drive)

### Goal fit

| Criterion | Verdict |
|---|---|
| Type | Product demo (screen capture) — correct for PH |
| Journey alignment | **Hero journey** `problem-plan-loop-drive` — correct |
| Operator home | Session Detail path (not Dashboard tourism) — correct |
| Proof of product | Real TUI, real keybindings/story from tape | **Pass** |

### Story arc (HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION)

| Beat | Expected | Observed / risk |
|---|---|---|
| HOOK (0–8s) | Pain / land Session Detail | Early frames can look empty (cold start / shell attach). **Risk:** first 5–10s lose scrollers |
| ORIENTATION | Session chrome, composer, ReAct | Present mid-film if seed history exists |
| ACTION | Workers / plan surfaces | Present when film hits plan-loop control plane |
| PROOF | DELEGATE / workers visible | Depends on seeded project; soft labels if empty |
| RESOLUTION | Clear takeaway | No end-card CTA (“Install” / tagline) |

### Marketing-video skill checks

| Rule | Status |
|---|---|
| Captions (85% mute watch) | **Fail** — no burned-in or soft captions |
| Wrong aspect for social | N/A if PH-only; **re-export 16:9** for YT |
| AI text in frame | N/A (real UI) |
| Over-producing | Good — authentic terminal is on-brand for Orchestrator ICP |
| Length for PH | **Borderline long** for cold PH; 48s is OK if first 10s prove value |

### Verdict — V1

| | |
|---|---|
| **Ship for PH video?** | **Yes — revised package shipped** |
| **Grade** | **B** product authenticity · **B** packaging (post-revision) |

**Implemented (2026-07-16)** in `media_pack/demo_render/`:

1. Trim cold open → start at **5s** of raw VHS through **40s** (~35s product).  
2. HTML title + end cards (html-video frames → PNG → 3s each).  
3. Caption overlays (HTML strips): Session Detail · workers · plans · specialists · resume.  
4. Export **1920×1080 16:9** → `ph_ready/hero-video-ph-ready.mp4` (**41s** total).  
5. Rebuild: `demo_render/build.sh`.

**Still optional:** light music bed; 9:16 social crop; re-film denser mid-proof with `SPUR_DEMO_STORY_PACE=1`.

**Do not** replace this film with V2 trailer on the PH listing.

---

## V2 — Marketing trailer (Seedance)

### Goal fit

| Criterion | Verdict |
|---|---|
| Type | AI-generated motion / hero visual |
| Channel | Social trailer, ads, site hero loop |
| Product truth | Start/end conditioned on real stills | **Partial** — motion can invent intermediate chrome |

### Marketing-video skill checks

| Rule | Status |
|---|---|
| Length for social | **Pass** (~8s) |
| Aspect 16:9 | **Pass** |
| Audio | **Present** (AAC) — verify it’s music not nonsense |
| Captions / readable AI text | **Risk** — models invent text; don’t rely on on-video copy |
| Authenticity for PH product proof | **Fail as product demo** — fine as *brand motion* |
| Platform | X/LinkedIn 16:9 OK; for TikTok/Reels need **9:16 reframe** |

### Verdict — V2

| | |
|---|---|
| **Ship for social?** | **Yes, with guardrails** |
| **Ship as PH product video?** | **No** |
| **Grade** | **B** social polish · **D** as product proof |

**Must-fix for social**

1. Pair every post with a **real** GIF or link to install — trailer alone invites “is this real UI?” comments.  
2. Captions in the post body (not in-video AI text): *Control tower for CLI coding agents.*  
3. If UI drifts too far from `gallery_stills/`, re-run Seedance with stronger stills or shorter duration (4–6s push-in only).  
4. Optional: 9:16 crop for vertical.

---

## Side-by-side recommendation

| Use case | Choose |
|---|---|
| Product Hunt listing video | **V1** (trimmed + end card) |
| Product Hunt gallery | **Stills from real pack** only |
| X / LinkedIn launch post | **V2** trailer + link to PH + one real still |
| Website hero | Loop **V2** or short clip of **V1** mid-proof |
| Ads | V2 as hook → land on real demo / install |

```
V1 (truth)  ── PH video, docs, technical audience
V2 (style)  ── social attention, brand motion
Never swap roles.
```

---

## Production backlog (ordered)

| # | Task | Owner | Done when |
|---:|---|---|---|
| 1 | Trim V1 to first proof frame; cut dead open | Launch / DevRel | First 3s show Session Detail UI |
| 2 | ffmpeg/Hyperframes end card on V1 | Creative | Tagline + install visible 2s |
| 3 | Export V1 1080p 16:9 for YouTube → PH | DevRel | Public unlisted URL |
| 4 | Post package: V2 + caption + PH link | Social | Draft ready T−1 |
| 5 | Optional re-film V1 with `SPUR_DEMO_STORY_PACE=1` if stills look sparse | Eng | denser mid-film proof |
| 6 | When CLI supports it: `brain_activity` scores on V1/V2 | Growth | numeric hook/sustain |

---

## Platform specs quick ref

| Platform | Ratio | Length | Asset |
|---|---|---|---|
| Product Hunt video | 16:9 preferred | ≤90s | V1 trimmed |
| YouTube | 16:9 | 30–60s | V1 |
| X / LinkedIn | 16:9 | ≤15s | V2 |
| Reels/TikTok | 9:16 | ≤15s | reframe V2 |

---

## Bottom line

- **V1 is the only honest PH product video** — authentic, journey-correct, under-packaged. Ship after trim + end card + captions.  
- **V2 is a good social trailer** — right length and polish; keep it off the PH product-proof slot.  
- Virality API blocked; re-run when `higgsfield generate create brain_activity` supports text models.
