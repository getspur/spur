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
  "assets": [
    {
      "asset_id": "demo-proof-01",
      "owner": "real-capture",
      "type": "video",
      "source_or_job_id": "D1F10781",
      "source_locator": {
        "start_seconds": 27.5,
        "end_seconds": 46.5
      },
      "claim_ids": ["claim-03"],
      "timeline_slot": {
        "start_seconds": 16,
        "end_seconds": 35
      },
      "approval_status": "approved",
      "rights_status": "owned"
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
- `approvals`: `concept_layout`, `script_storyboard`, and `paid_generation` are each `approved`.
- `assets`: a nonempty array with unique `asset_id` values. Every asset has a nonempty string `asset_id`, `type`, and `source_or_job_id`; `owner` is exactly `open-design`, `html-video`, `higgsfield`, `palmier`, or `real-capture`; `approval_status` is `approved`; and `rights_status` is `owned` or `cleared`.
- `delivery`: `path` is a nonempty string; `duration_seconds`, `width`, `height`, and `fps` are positive numbers; `checksum_sha256` is exactly 64 lowercase hexadecimal characters.

Optional fields include asset duration, aspect ratio, resolution, prompt revision, voice, and Palmier clip IDs. A schema failure reports the generic error `manifest violates the explainer delivery contract`; inspect all required fields, types, exact values, uniqueness, and checksum format to locate the cause.

Run:

```sh
scripts/validate-delivery.sh MANIFEST.json VIDEO.mp4
```

The supplied `VIDEO.mp4` path must exactly equal `delivery.path`. The matching file must also satisfy the declared duration, dimensions, frame rate, and checksum, contain readable video and audio streams, and pass strict full-decode validation.

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
