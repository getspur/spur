# Explainer Handoff Contract

Read this when building the scene ownership map, importing into PalmierPro, recovering a failed stage, or validating delivery.

## Scene and asset owner rule

Assign every asset exactly one primary owner and every scene exactly one primary owner.

| Owner | Owns | Asset boundary |
|---|---|---|
| `real-capture` | Product proof, real UI, interactions, testimony | Never invent or reconstruct behavior |
| `open-design` | Brief, claim register, layout, storyboard | Never render timeline media |
| `html-video` | Deterministic diagrams, motion typography, concept plates | Never supply factual UI evidence or final assembly |
| `higgsfield` | Dry narration, metaphorical footage, atmospheric inserts | Never supply readable UI, product claims, logos, titles, or final assembly |
| `palmier` | Native text, captions, transitions, mix, color, final timeline | Never perform unapproved generation or factual invention |

Every scene has one primary owner. If Palmier composites differently owned input assets, Palmier is the scene owner while inputs retain their own asset owners. If one asset appears to need two owners, split it before Palmier assembly.

Close the traceability graph before delivery:

```text
eligible source asset <- claim.source_asset_ids <- claim <- scene.claim_ids <- scene -> scene.asset_ids -> timeline asset
```

Claims therefore identify their factual source assets, scenes identify the claims they use, and scenes identify every production asset placed on the timeline.

## Manifest schema

Use schema version 1:

```json
{
  "schema_version": 1,
  "project": "product-control-loop",
  "route": "enhance",
  "approvals": {
    "concept_layout": "approved",
    "script_storyboard": "approved",
    "paid_generation": "approved"
  },
  "claims": [
    {
      "claim_id": "claim-03",
      "text": "The product shows the control loop in the real demo.",
      "source_asset_ids": ["brief-source-01", "demo-proof-01"]
    }
  ],
  "assets": [
    {
      "asset_id": "brief-source-01",
      "owner": "open-design",
      "type": "document",
      "source_or_job_id": "approved-brief-v3",
      "approval_status": "approved",
      "rights_status": "cleared"
    },
    {
      "asset_id": "demo-proof-01",
      "owner": "real-capture",
      "type": "video",
      "source_or_job_id": "D1F10781",
      "source_locator": {
        "start_seconds": 27.5,
        "end_seconds": 46.5
      },
      "approval_status": "approved",
      "rights_status": "owned"
    },
    {
      "asset_id": "control-loop-plate-01",
      "owner": "html-video",
      "type": "video",
      "source_or_job_id": "notebook-plate-v2",
      "approval_status": "approved",
      "rights_status": "owned"
    },
    {
      "asset_id": "narration-01",
      "owner": "higgsfield",
      "type": "audio",
      "source_or_job_id": "voice-job-1842",
      "prompt_or_script_revision": "script-v5",
      "approval_status": "approved",
      "rights_status": "cleared"
    },
    {
      "asset_id": "end-card-title-01",
      "owner": "palmier",
      "type": "native-text",
      "source_or_job_id": "palmier-title-clip-7",
      "approval_status": "approved",
      "rights_status": "owned"
    }
  ],
  "scenes": [
    {
      "scene_id": "scene-proof",
      "owner": "real-capture",
      "timeline_slot": {"start_seconds": 16, "end_seconds": 35},
      "asset_ids": ["demo-proof-01"],
      "claim_ids": ["claim-03"]
    },
    {
      "scene_id": "scene-concept",
      "owner": "html-video",
      "timeline_slot": {"start_seconds": 35, "end_seconds": 45},
      "asset_ids": ["control-loop-plate-01"],
      "claim_ids": []
    },
    {
      "scene_id": "scene-voice",
      "owner": "higgsfield",
      "timeline_slot": {"start_seconds": 0, "end_seconds": 60},
      "asset_ids": ["narration-01"],
      "claim_ids": []
    },
    {
      "scene_id": "scene-end-card",
      "owner": "palmier",
      "timeline_slot": {"start_seconds": 45, "end_seconds": 60},
      "asset_ids": ["end-card-title-01"],
      "claim_ids": []
    }
  ],
  "delivery": {
    "path": "/deliveries/product-control-loop/product-control-loop-v1.mp4",
    "duration_seconds": 60,
    "width": 1920,
    "height": 1080,
    "fps": 30,
    "checksum_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  }
}
```

Required fields and exact constraints:

- Root: `schema_version` is `1`; `project` is a nonempty string; `route` is `create` or `enhance`.
- `approvals`: an object whose exact sorted key set is `concept_layout`, `paid_generation`, and `script_storyboard`; every value is exactly `approved`. Extra gates are invalid.
- `claims`: a nonempty array with unique nonempty `claim_id` values. Every claim has nonempty `text` and nonempty unique `source_asset_ids`; each source ID resolves to an existing `real-capture` or `open-design` asset.
- `assets`: a nonempty array with unique `asset_id` values. Every asset has a nonempty string `asset_id`, `type`, and `source_or_job_id`; `owner` is exactly `open-design`, `html-video`, `higgsfield`, `palmier`, or `real-capture`; `approval_status` is `approved`; and `rights_status` is `owned` or `cleared`.
- `real-capture` assets: require numeric `source_locator.start_seconds >= 0` and `source_locator.end_seconds > source_locator.start_seconds`.
- `higgsfield` assets: require a nonempty `prompt_or_script_revision`.
- `scenes`: a nonempty array with unique nonempty `scene_id` values. Each scene has one scalar `owner` from `real-capture`, `html-video`, `higgsfield`, or `palmier`; a numeric `timeline_slot` beginning at or after zero, ending after it begins, and ending no later than `delivery.duration_seconds`; nonempty unique known non-`open-design` `asset_ids`; and an array of unique known `claim_ids`. Non-factual scenes may use an empty `claim_ids` array.
- Scene ownership: scenes never reference `open-design` assets. Every asset in a non-`palmier` scene has the same owner as the scene. A `palmier` scene may composite mixed-owner timeline inputs. Every non-`open-design` asset appears in at least one scene, and every claim is used by at least one scene.
- `delivery`: `path` is a nonempty string; `duration_seconds`, `width`, `height`, and `fps` are positive numbers; `checksum_sha256` is exactly 64 lowercase hexadecimal characters.

Optional asset metadata may include `duration_seconds`, `aspect_ratio`, `width`, `height`, `fps`, `audio_format`, voice, and Palmier clip IDs. A schema failure reports the generic error `manifest violates the explainer delivery contract`; inspect all required fields, types, exact values, uniqueness, references, ownership, coverage, and `checksum_sha256` format to locate the cause.

Run:

```sh
crates/spur-cli/assets/skills/explainer-video-editor/scripts/validate-delivery.sh MANIFEST.json VIDEO.mp4
```

The supplied `VIDEO.mp4` path must exactly equal `delivery.path`. The matching file must also satisfy the declared duration, width, height, frame rate, and `checksum_sha256`, contain H.264 video and AAC audio streams, and pass strict full-decode validation.

## Three gates

| Gate | Required decision |
|---|---|
| `concept_layout` | Approve the palette, typography system, composition language, and pacing envelope |
| `script_storyboard` | Approve the narration/script, storyboard, source selects, and scene ownership map |
| `paid_generation` | Approve paid generation |

There is no fourth production gate. The final export is a review artifact, and reversible Palmier edits do not require per-edit approval.

## Asset requirements

- **HTML plates:** Self-contained, deterministic, exact duration, and no baked voice-over.
- **Narration:** One voice, dry, no music, scene-aligned, with pronunciation notes.
- **Generated footage:** Metaphorical; no dialogue, captions, readable UI, claims, logos, or watermarks.
- **Real capture:** Inspect by content and record exact source start and end seconds.
- **Palmier text:** Create names, claims, CTA, and captions natively in Palmier.

## Recovery matrix

| Failure | Recovery |
|---|---|
| Unsupported claim | Block the claim until a source is attached or the claim is removed. |
| Weak demo | Search the real source for stronger product proof; do not reconstruct it. |
| Notebook capture failure | Use the notebook cell MIME output and capture port as the source of truth; verify trust, text/HTML MIME, canvas availability, and port access, then rerender only the affected plate. |
| Narration runs long | Tighten one segment and preserve scene alignment. |
| Higgsfield timeout | Rejoin the existing job; do not start a duplicate. |
| Two equivalent failures | Revise the prompt; renew approval when cost or scope changes. |
| Invented UI, text, or logo | Reject the asset. |
| Higgsfield assembler unavailable | Continue final assembly in Palmier. |
| Palmier state is stale | Reread the active Palmier timeline, preserve successful edits, and retry the smallest failed mutation. |
| Export queue warning | Inspect the `manage_exports` warning and result; accept only a successful terminal result. |

## Delivery checklist

- [ ] Duration is correct; there are no gaps or flashes.
- [ ] Every claim is sourced, and all product behavior is real.
- [ ] Narration is intelligible.
- [ ] Text is safe, correct, and within frame-safe bounds.
- [ ] Representative frames have been inspected.
- [ ] `validate-delivery` passes.
- [ ] Deliver the MP4, Palmier project, notebook, and manifest.
- [ ] Include captions, ProRes, music, or alternate ratios only when requested.
