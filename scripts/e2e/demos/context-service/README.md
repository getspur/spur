# SPUR Context Service — product demo storyboard

Full marketing path aligned with
[`PRODUCT_AND_USAGE.md`](../../../../crates/spur-context-service/docs/PRODUCT_AND_USAGE.md):

1. **Terminal setup** — `spur context` CLI (`auth` / `key` / `mcp`)
2. **Terminal tools** — `external_knowledge_context` → `external_code_read`
3. **Higgsfield** — 4-beat film grounded on VHS terminal captures as refs

## Layout

```text
bin/demo-context-setup.sh      # CLI setup demo (real spur binary)
bin/demo-external-tools.sh     # multi-round external_* (fixture | live)
fixtures/                      # warm serde@1.0.197 responses for fixture mode
tapes/                         # VHS marketing captures → mp4/gif
render.sh                      # run demos + VHS + extract still frames
generate-higgsfield.sh         # Seedance 2.0 with frame refs
out/                           # gitignored artifacts
```

## Quick start

```bash
# From repo root (or anywhere; scripts self-locate)
cd scripts/e2e/demos/context-service

# 1–2) Capture terminal media + stills
./render.sh

# 3) Marketing film (requires higgsfield auth login)
./generate-higgsfield.sh
```

### Live external_* (optional)

```bash
export SPUR_DEMO_MODE=live
export SPUR_CONTEXT_SERVICE_API_KEY='…'   # or: spur context key use <id>
# optional: SPUR_DEMO_PROFILE=<public-key-id>
SPUR_DEMO_PAUSE=0.4 ./bin/demo-external-tools.sh
```

Fixture mode is the default so marketing captures stay reproducible offline.

## Storyboard beats (Higgsfield)

| Beat | Time | Story |
|------|------|--------|
| 1 | 0–3s | Problem — weak dependency context |
| 2 | 3–6s | Setup + index — package → code graph |
| 3 | 6–9s | Tools — knowledge pack → code read |
| 4 | 9–12s | Two planes + CTA — worktree vs external |

## Notes

- These tapes are **marketing demos**, not part of the TUI golden suite in
  `scripts/e2e/vhs/`. Do not add them to `run-vhs-suite.sh` without a separate
  policy for media artifacts.
- `out/` is gitignored. Keep fixtures under `fixtures/` if you refresh recorded
  tool payloads.
