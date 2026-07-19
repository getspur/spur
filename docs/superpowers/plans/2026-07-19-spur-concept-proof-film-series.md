# SPUR Concept Proof Film Series Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce three 40-second SPUR proof films that animate one product concept, match-cut into existing real TUI evidence, and remain traceable to approved or visibly diagnostic source material.

**Architecture:** A versioned series manifest in the SPUR worktree records story timing, source identity, proof terms, and promotability. The open html-video Jute notebook uses one shared deterministic Anime.js/canvas engine plus three declarative story configurations to render interactive storyboards and sequential 16-second concept plates through the existing `spur-ad-capture` port. Palmier Pro creates three new timelines from those plates, reviewed real media, the existing music bed, and the existing domain-free end card; existing V2 timelines and exports are never changed.

**Tech Stack:** Jute Notebook MCP, Open Design, html-video, Anime.js 4.4.1, Palmier Pro MCP, Bash, jq, ffmpeg/ffprobe, Git.

---

## File structure

- Create: `docs/product_launch/media_pack/concept-proof-series-manifest.json`
  - Machine-readable evidence, story, timing, and output contract.
- Create: `docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh`
  - Contract checks for source identity, notebook handoff, output streams, exact durations, and forbidden copy.
- Modify through Notebook MCP only: `/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb`
  - Interactive storyboard, shared motion engine, sequential capture, render control, and concept-plate preview.
- Create: `docs/product_launch/media_pack/ph_ready/series/motion/spur-control-loop-concept-v3-16s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/series/motion/spur-durable-memory-concept-v3-16s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/series/motion/spur-acp-agents-concept-v3-16s.mp4`
  - Exact 16-second concept and match-cut plates.
- Create: `docs/product_launch/media_pack/ph_ready/series/spur-control-loop-proof-40s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/series/spur-durable-memory-proof-40s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/series/spur-acp-agents-proof-40s.mp4`
  - Final Palmier exports.
- Modify through Notebook MCP only: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
  - Final series delivery record, hashes, timeline IDs, proof status, and interactive handoff.

## Locked Palmier inputs

- Project: `SPUR Product Hunt Hero - Real TUI`
- Music: `23DF6A98` (`music-45s`, use source seconds `[0, 40]`)
- Domain-free end card: `5BECDF39` (`Anime end card V2 - 5s`)
- Approved Session Detail: `791B452C`
- Approved worker visibility: `14D82963`
- Approved plan state: `82D9D60A`
- Approved specialist routing: `63605F31`
- Approved session resume: `4B29113A`
- Diagnostic four-agent TUI: `F2C142AD`
- Diagnostic source SHA256: `b5c407a3753bae990b0cdf95fd5dac2c747934e15f8a314aaff42e52bf83ecb5`
- Existing V2 timelines to preserve: `96C176C3`, `DC2C64B9`
- Working roster: Claude Code, Grok, Codex, and OpenCode

---

### Task 1: Add the red series contract

**Files:**
- Create: `docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh`
- Test: `docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh`

- [ ] **Step 1: Create the failing contract**

Use `apply_patch` to create the executable shell contract below:

```bash
#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/concept-proof-series-manifest.json"
NOTEBOOK="$ROOT/product-hunt-media-pack.ipynb"
failures=0

pass() { printf 'PASS %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; failures=$((failures + 1)); }
require() {
  command -v "$1" >/dev/null || {
    printf 'missing tool: %s\n' "$1" >&2
    exit 2
  }
}

for tool in jq ffprobe ffmpeg rg shasum awk; do require "$tool"; done

[[ -f "$MANIFEST" ]] && pass "series manifest exists" || fail "series manifest exists"

if [[ -f "$MANIFEST" ]]; then
  jq -e '
    .version == 1 and
    .fps == 30 and
    .canvas == {"width":1920,"height":1080} and
    (.films | length == 3) and
    all(.films[];
      .duration_seconds == 40 and
      .duration_frames == 1200 and
      .chapters == {"hook":[0,90],"concept":[90,390],"match":[390,480],"proof":[480,1050],"end":[1050,1200]} and
      (.proof_sources | map(.duration_seconds) | add) == 19
    )
  ' "$MANIFEST" >/dev/null \
    && pass "series timing schema" \
    || fail "series timing schema"

  for source_id in session-detail worker-visibility plan-state specialist-routing session-resume; do
    path="$(jq -r --arg id "$source_id" '.sources[$id].path // empty' "$MANIFEST")"
    checksum="$(jq -r --arg id "$source_id" '.sources[$id].sha256 // empty' "$MANIFEST")"
    if [[ -z "$path" || -z "$checksum" || ! -f "$ROOT/$path" ]]; then
      fail "$source_id approved source exists"
      continue
    fi
    actual="$(shasum -a 256 "$ROOT/$path" | awk '{print $1}')"
    [[ "$actual" == "$checksum" ]] \
      && pass "$source_id approved checksum" \
      || fail "$source_id approved checksum"
  done

  jq -e '
    .sources["four-agent-diagnostic"].status == "diagnostic" and
    .sources["four-agent-diagnostic"].sha256 == "b5c407a3753bae990b0cdf95fd5dac2c747934e15f8a314aaff42e52bf83ecb5" and
    all(.films[] | .proof_sources[] | select(.source_id == "four-agent-diagnostic"); .watermark == true)
  ' "$MANIFEST" >/dev/null \
    && pass "diagnostic source remains watermarked" \
    || fail "diagnostic source remains watermarked"
fi

notebook_source="$(jq -r '.cells[].source | if type == "array" then join("") else . end' "$NOTEBOOK")"
notebook_html="$(jq -r '.cells[].outputs[]? | .data["text/html"]? // empty | if type == "array" then join("") else . end' "$NOTEBOOK")"

for phrase in \
  'Delegate deeply. Keep the decision.' \
  'The agent can stop. The work remains.' \
  'Choose the agent. Keep one control system.' \
  'INSTALL SPUR · COMMUNITY FREE'; do
  [[ "$notebook_source$notebook_html" == *"$phrase"* ]] \
    && pass "notebook series copy: $phrase" \
    || fail "notebook series copy: $phrase"
done

if rg -qi --glob '!concept-proof-series-contract.test.sh' 'otobank' "$ROOT"; then
  fail "series contains no unrelated project copy"
else
  pass "series contains no unrelated project copy"
fi

while IFS=$'\t' read -r output expected_frames; do
  file="$ROOT/$output"
  if [[ ! -f "$file" ]]; then
    fail "final output exists: $output"
    continue
  fi
  video="$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,avg_frame_rate,nb_frames -of csv=p=0 "$file")"
  audio="$(ffprobe -v error -select_streams a:0 -show_entries stream=codec_name,channels,sample_rate -of csv=p=0 "$file")"
  duration="$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file")"
  [[ "$video" == "h264,1920,1080,30/1,$expected_frames" ]] \
    && pass "$output video contract" \
    || fail "$output video contract ($video)"
  [[ "$audio" == "aac,48000,2" ]] \
    && pass "$output audio contract" \
    || fail "$output audio contract ($audio)"
  [[ "$duration" == "40.000000" ]] \
    && pass "$output duration" \
    || fail "$output duration ($duration)"
  ffmpeg -v error -i "$file" -f null - \
    && pass "$output full decode" \
    || fail "$output full decode"
done < <(jq -r '.films[] | [.output, (.duration_frames|tostring)] | @tsv' "$MANIFEST" 2>/dev/null || true)

if (( failures > 0 )); then
  printf '\n%d series contract failure(s)\n' "$failures" >&2
  exit 1
fi

printf '\nAll concept-proof series contracts passed\n'
```

- [ ] **Step 2: Make the contract executable**

Run:

```bash
chmod +x docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh
```

Expected: exit 0.

- [ ] **Step 3: Run the contract and verify the intended red state**

Run:

```bash
bash docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh
```

Expected: FAIL with `series manifest exists` and the three missing series-copy assertions. The command must fail before any implementation exists.

- [ ] **Step 4: Commit the red contract**

```bash
git add docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh
git commit -m "test(product-launch): D4.bg define concept proof series contract"
```

### Task 2: Materialize the evidence and timing manifest

**Files:**
- Create: `docs/product_launch/media_pack/concept-proof-series-manifest.json`
- Test: `docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh`

- [ ] **Step 1: Create the manifest**

Use `apply_patch` with this complete JSON:

```json
{
  "version": 1,
  "fps": 30,
  "canvas": { "width": 1920, "height": 1080 },
  "anime_version": "4.4.1",
  "end_card": {
    "media_ref": "5BECDF39",
    "copy": "INSTALL SPUR · COMMUNITY FREE",
    "duration_seconds": 5
  },
  "music": { "media_ref": "23DF6A98", "source_seconds": [0, 40] },
  "sources": {
    "session-detail": {
      "status": "approved",
      "path": "live_demos/13-problem-plan-loop-drive.mp4",
      "sha256": "4d94c2c9d320eb53b4cd4bb56f0bddac337239ee4419e6a3ffc31b47649797d9",
      "media_ref": "791B452C",
      "proof_terms": ["INSERT", "following"]
    },
    "worker-visibility": {
      "status": "approved",
      "path": "live_demos/10-problem-ops-visibility.mp4",
      "sha256": "4c252847c6498d6be5d7f581c79c0f06665a7fef90f12eb589811fefc207991c",
      "media_ref": "14D82963",
      "proof_terms": ["WORKERS", "worker", "running"]
    },
    "plan-state": {
      "status": "approved",
      "path": "live_demos/11-problem-plan-progress.mp4",
      "sha256": "011f20addf6850055a9bd062521d22ca94a440898eaf4ec6d5c29c7630335407",
      "media_ref": "82D9D60A",
      "proof_terms": ["Plans", "No plans found"]
    },
    "specialist-routing": {
      "status": "approved",
      "path": "live_demos/09-product-e2e-flow.mp4",
      "sha256": "7fd8473a7870afff7b5085c6a00ef306ac257b0021d8f150884886caa84d47ec",
      "media_ref": "63605F31",
      "proof_terms": ["agent=", "model=", "effort="]
    },
    "session-resume": {
      "status": "approved",
      "path": "live_demos/04-session-resume.mp4",
      "sha256": "cb110d2cfa9149cb9d8344987f03f11852a181926ee85a572bebf8dbdff0660c",
      "media_ref": "4B29113A",
      "proof_terms": ["Session", "Resumed from prior conversation"]
    },
    "four-agent-diagnostic": {
      "status": "diagnostic",
      "media_ref": "F2C142AD",
      "sha256": "b5c407a3753bae990b0cdf95fd5dac2c747934e15f8a314aaff42e52bf83ecb5",
      "proof_terms": ["Claude Code", "Grok", "Codex", "OpenCode"]
    }
  },
  "films": [
    {
      "id": "control-loop",
      "title": "Keep the control loop",
      "takeaway": "Delegate deeply. Keep the decision.",
      "duration_seconds": 40,
      "duration_frames": 1200,
      "chapters": { "hook": [0, 90], "concept": [90, 390], "match": [390, 480], "proof": [480, 1050], "end": [1050, 1200] },
      "concept_output": "ph_ready/series/motion/spur-control-loop-concept-v3-16s.mp4",
      "output": "ph_ready/series/spur-control-loop-proof-40s.mp4",
      "proof_sources": [
        { "source_id": "four-agent-diagnostic", "source_seconds": [93, 112], "duration_seconds": 19, "watermark": true }
      ]
    },
    {
      "id": "durable-memory",
      "title": "Work survives the session",
      "takeaway": "The agent can stop. The work remains.",
      "duration_seconds": 40,
      "duration_frames": 1200,
      "chapters": { "hook": [0, 90], "concept": [90, 390], "match": [390, 480], "proof": [480, 1050], "end": [1050, 1200] },
      "concept_output": "ph_ready/series/motion/spur-durable-memory-concept-v3-16s.mp4",
      "output": "ph_ready/series/spur-durable-memory-proof-40s.mp4",
      "proof_sources": [
        { "source_id": "session-detail", "source_seconds": [8, 18], "duration_seconds": 10, "watermark": false },
        { "source_id": "session-resume", "source_seconds": [0, 9], "duration_seconds": 9, "watermark": false }
      ]
    },
    {
      "id": "acp-agents",
      "title": "Bring any ACP agent",
      "takeaway": "Choose the agent. Keep one control system.",
      "duration_seconds": 40,
      "duration_frames": 1200,
      "chapters": { "hook": [0, 90], "concept": [90, 390], "match": [390, 480], "proof": [480, 1050], "end": [1050, 1200] },
      "concept_output": "ph_ready/series/motion/spur-acp-agents-concept-v3-16s.mp4",
      "output": "ph_ready/series/spur-acp-agents-proof-40s.mp4",
      "proof_sources": [
        { "source_id": "specialist-routing", "source_seconds": [49, 59], "duration_seconds": 10, "watermark": false },
        { "source_id": "four-agent-diagnostic", "source_seconds": [93, 102], "duration_seconds": 9, "watermark": true }
      ]
    }
  ]
}
```

- [ ] **Step 2: Validate source identity and timing independently of missing outputs**

Run:

```bash
jq -e '.films | length == 3 and all(.[]; (.proof_sources | map(.duration_seconds) | add) == 19)' docs/product_launch/media_pack/concept-proof-series-manifest.json
while IFS=$'\t' read -r path checksum; do
  test "$(shasum -a 256 "docs/product_launch/media_pack/$path" | awk '{print $1}')" = "$checksum"
done < <(jq -r '.sources[] | select(.status == "approved") | [.path,.sha256] | @tsv' docs/product_launch/media_pack/concept-proof-series-manifest.json)
```

Expected: both commands exit 0.

- [ ] **Step 3: Run the contract and confirm remaining failures are implementation outputs**

Run:

```bash
bash docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh
```

Expected: manifest and source checks PASS; notebook copy and three final output checks still FAIL.

- [ ] **Step 4: Commit the manifest**

```bash
git add docs/product_launch/media_pack/concept-proof-series-manifest.json
git commit -m "docs(product-launch): D4.bh materialize concept proof evidence"
```

### Task 3: Build the interactive notebook storyboard and shared engine

**Files:**
- Modify through Notebook MCP only: `/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb`
- Read: `docs/product_launch/media_pack/concept-proof-series-manifest.json`

- [ ] **Step 1: Open and orient to the html-video app**

Call:

```text
notebook_open(path="/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb")
notebook_context_pack()
notebook_snapshot()
```

Expected: the notebook contains `spur-ad-capture`, `spur-ad-render`, and `spur-ad-video-embed`; the existing source port is `spur-ad-capture`.

- [ ] **Step 2: Insert the approved brief cell**

Use `notebook_insert_cell(kind="markdown", after_id="html-video-title", source=...)` with this content:

```markdown
## SPUR concept proof series - locked brief

- Three independent 40-second films: control loop, durable memory, ACP agents.
- Each film: 3s hook, 10s concept, 3s match cut, 19s real proof, 5s end card.
- Match animation geometry to existing TUI evidence; never invent product proof.
- Working roster: Claude Code, Grok, Codex, OpenCode.
- Unapproved four-agent footage remains `DRAFT - DIAGNOSTIC CAPTURE`.
- End card: `INSTALL SPUR · COMMUNITY FREE`; no external domain.
```

Expected: one new markdown cell and no production cell changes.

- [ ] **Step 3: Insert the shared engine cell**

Insert a JavaScript cell with ID recorded as `ENGINE_CELL_ID`. The cell must read the worktree manifest at build time, cache Anime.js 4.4.1, and expose one builder with this exact interface:

```javascript
await (async () => {
  const manifestPath = "/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/concept-proof-series-manifest.json";
  const manifest = JSON.parse(await Deno.readTextFile(manifestPath));
  const cacheKey = "__spurAnimeUmdV441";
  let animeBundle = globalThis[cacheKey];
  if (!animeBundle) {
    const response = await fetch("https://cdn.jsdelivr.net/npm/animejs@4.4.1/dist/bundles/anime.umd.min.js");
    if (!response.ok) throw new Error("Anime.js 4.4.1 fetch failed: " + response.status);
    animeBundle = await response.text();
    if (!animeBundle.includes("@version v4.4.1")) throw new Error("Unexpected Anime.js bundle");
    globalThis[cacheKey] = animeBundle;
  }
  const safeBundle = animeBundle.replace(/<\/script/gi, "<\\/script");

  const stories = {
    "control-loop": {
      eyebrow: "KEEP THE CONTROL LOOP",
      hook: "ONE REQUEST. HIDDEN PARALLEL WORK.",
      takeaway: "DELEGATE DEEPLY. KEEP THE DECISION.",
      nodes: ["USER", "BRAIN", "CLAUDE CODE", "GROK", "CODEX", "OPENCODE"],
      verbs: ["submit_plan", "delegate", "review", "resume"],
      accent: "#957FB8"
    },
    "durable-memory": {
      eyebrow: "WORK SURVIVES THE SESSION",
      hook: "THE AGENT STOPS MID-TASK.",
      takeaway: "THE AGENT CAN STOP. THE WORK REMAINS.",
      nodes: ["SESSION", "PLAN", "LINEAGE", "EVIDENCE", "REVIEW", "RESUME"],
      verbs: ["disconnect", "persist", "reconnect", "resume"],
      accent: "#80BFA3"
    },
    "acp-agents": {
      eyebrow: "BRING ANY ACP AGENT",
      hook: "FOUR AGENTS. FOUR DIFFERENT WORKFLOWS.",
      takeaway: "CHOOSE THE AGENT. KEEP ONE CONTROL SYSTEM.",
      nodes: ["CLAUDE CODE", "GROK", "CODEX", "OPENCODE", "ACP", "SPUR"],
      verbs: ["agent=", "model=", "effort=", "dispatch"],
      accent: "#D0A85C"
    }
  };

  function buildFilmHtml({ filmId, capture }) {
    const story = stories[filmId];
    if (!story) throw new Error("Unknown proof film: " + filmId);
    const duration = 16000;
    const bundle = safeBundle;
    const dataCapture = capture
      ? ' data-capture="true" data-capture-duration-sec="16" data-capture-fps="30"'
      : "";
    const html = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=1920,height=1080,initial-scale=1">
<style>*{box-sizing:border-box}html,body{margin:0;width:100%;height:100%;overflow:hidden;background:#0B0E14}body{display:grid;place-items:center}.stage{position:relative;width:min(100vw,1920px);aspect-ratio:16/9;background:#0B0E14}.fallback,canvas{position:absolute;inset:0;width:100%;height:100%}.fallback{display:grid;place-items:center;color:#E6E1CF;font:800 72px Inter,sans-serif}.fallback span{color:#7FB4CA}canvas{display:block}</style></head><body>
<div class="stage"><div class="fallback">${story.eyebrow}<span>${story.takeaway}</span></div><canvas${dataCapture} width="1920" height="1080"></canvas></div>
<script>${bundle}</script><script>
(() => {
  const story=${JSON.stringify(story)};
  const ctx=document.querySelector('canvas').getContext('2d');
  const W=1920,H=1080,D=16000,C={ink:'#0B0E14',surface:'#111620',ivory:'#E6E1CF',cyan:'#7FB4CA',line:'#2A2E38',muted:'#AEB6C5'};
  const clamp=(v,a,b)=>Math.max(a,Math.min(b,v));
  const ease=t=>t<.5?2*t*t:1-Math.pow(-2*t+2,2)/2;
  const phase=(t,a,b)=>ease(clamp((t-a)/(b-a),0,1));
  const state={hook:0,concept:0,match:0};
  const motion=anime.createTimeline({autoplay:false});
  motion.add(state,{hook:1,duration:3000},0)
    .add(state,{concept:1,duration:10000},3000)
    .add(state,{match:1,duration:3000},13000);
  function rr(x,y,w,h,r,fill,stroke){ctx.beginPath();ctx.roundRect(x,y,w,h,r);if(fill){ctx.fillStyle=fill;ctx.fill()}if(stroke){ctx.strokeStyle=stroke;ctx.lineWidth=2;ctx.stroke()}}
  function label(v,x,y,size,color,align='center',weight=700,alpha=1,mono=false){ctx.save();ctx.globalAlpha=alpha;ctx.fillStyle=color;ctx.textAlign=align;ctx.textBaseline='middle';ctx.font=weight+' '+size+'px '+(mono?'SFMono-Regular,Menlo,monospace':'Inter,sans-serif');ctx.fillText(v,x,y);ctx.restore()}
  function base(){ctx.fillStyle=C.ink;ctx.fillRect(0,0,W,H);const glow=ctx.createRadialGradient(1480,160,20,1480,160,620);glow.addColorStop(0,'rgba(127,180,202,'+(.06+.08*state.concept)+')');glow.addColorStop(1,'rgba(11,14,20,0)');ctx.fillStyle=glow;ctx.fillRect(0,0,W,H);ctx.strokeStyle='rgba(42,46,56,.5)';for(let x=0;x<W;x+=96){ctx.beginPath();ctx.moveTo(x,0);ctx.lineTo(x,H);ctx.stroke()}for(let y=0;y<H;y+=96){ctx.beginPath();ctx.moveTo(0,y);ctx.lineTo(W,y);ctx.stroke()}label('SPUR / CONCEPT PROOF SERIES',48,44,20,C.ivory,'left',700,1,true);label(story.eyebrow,W-48,44,20,C.cyan,'right',700,.7+.3*state.hook,true)}
  function node(text,x,y,w,a,color){ctx.save();ctx.globalAlpha=a;rr(x,y,w,110,4,'rgba(17,22,32,.96)',color);label(text,x+w/2,y+55,24,C.ivory,'center',750,1,true);ctx.restore()}
  function drawControl(t){const hook=phase(t,0,3000),split=phase(t,3000,6500),ret=phase(t,6500,10500),match=phase(t,13000,16000);label(story.hook,960,170,36,C.ivory,'center',750,1-hook*.35,true);node('USER REQUEST',150,300,360,1,C.cyan);node('BRAIN',780,300,360,split,C.ivory);['CLAUDE CODE','GROK','CODEX','OPENCODE'].forEach((v,i)=>node(v,1280,180+i*150,440,split,story.accent));ctx.strokeStyle=C.cyan;ctx.lineWidth=6;ctx.beginPath();ctx.moveTo(510,355);ctx.lineTo(780,355);ctx.stroke();ctx.strokeStyle=story.accent;for(let i=0;i<4;i++){ctx.beginPath();ctx.moveTo(1140,355);ctx.lineTo(1280,235+i*150);ctx.stroke()}label('submit_plan · delegate · review · resume',960,840,28,C.ivory,'center',700,ret,true);ctx.save();ctx.globalAlpha=match;ctx.strokeStyle=C.cyan;ctx.lineWidth=4;ctx.strokeRect(96,102,1728,876);ctx.restore()}
  function drawMemory(t){const drop=phase(t,0,3000),persist=phase(t,3000,9000),resume=phase(t,9000,13000),match=phase(t,13000,16000);label(story.hook,960,170,36,C.ivory,'center',750,1,true);node('AGENT PROCESS',180,300,420,1-drop*.85,story.accent);['PLAN','LINEAGE','EVIDENCE','REVIEW'].forEach((v,i)=>node(v,720,210+i*145,500,persist,C.cyan));ctx.strokeStyle=C.cyan;ctx.lineWidth=7;ctx.beginPath();ctx.moveTo(650,180);ctx.lineTo(650,850);ctx.stroke();node('RESUMED SESSION',1320,430,420,resume,C.ivory);label('disconnect  →  persist  →  reconnect  →  resume',960,880,28,C.ivory,'center',700,resume,true);ctx.save();ctx.globalAlpha=match;ctx.strokeStyle=C.cyan;ctx.lineWidth=4;ctx.strokeRect(80,100,1760,880);ctx.restore()}
  function drawAcp(t){const diverge=phase(t,0,3000),ports=phase(t,3000,9000),route=phase(t,9000,13000),match=phase(t,13000,16000);label(story.hook,960,160,34,C.ivory,'center',750,1,true);['CLAUDE CODE','GROK','CODEX','OPENCODE'].forEach((v,i)=>node(v,120,220+i*155,420,diverge,C.ivory));node('ACP',780,430,360,ports,C.cyan);node('SPUR',1380,430,360,route,story.accent);ctx.strokeStyle=C.cyan;ctx.lineWidth=6;for(let i=0;i<4;i++){ctx.beginPath();ctx.moveTo(540,275+i*155);ctx.lineTo(780,485);ctx.stroke()}ctx.beginPath();ctx.moveTo(1140,485);ctx.lineTo(1380,485);ctx.stroke();label('agent=  ·  model=  ·  effort=  ·  dispatch',960,870,28,C.ivory,'center',700,route,true);ctx.save();ctx.globalAlpha=match;ctx.strokeStyle=C.cyan;ctx.lineWidth=4;ctx.strokeRect(72,100,1776,880);ctx.restore()}
  function draw(ms){motion.seek(clamp(ms,0,D),true);base();if('${filmId}'==='control-loop')drawControl(ms);else if('${filmId}'==='durable-memory')drawMemory(ms);else drawAcp(ms);label(ms<3000?'PROBLEM':ms<13000?'CONCEPT':ms<16000?'MATCH CUT':'',48,H-42,18,C.muted,'left',650,1,true);label((ms/1000).toFixed(3)+' / 16.000',W-48,H-42,18,C.cyan,'right',700,1,true)}
  window.__hf={seek:s=>draw(clamp(s*1000,0,D))};draw(0);const start=performance.now();function tick(now){const ms=clamp(now-start,0,D);draw(ms);if(ms<D)requestAnimationFrame(tick)}requestAnimationFrame(tick);
})();</script></body></html>`;
    return html;
  }

  globalThis.__spurProofSeries = { manifest, stories, buildFilmHtml };
  return { [Symbol.for("Jupyter.display")]: () => ({ "text/html": "<div style='font:700 13px ui-monospace;color:#7FB4CA;background:#0B0E14;padding:14px'>SPUR proof-film engine ready · Anime.js 4.4.1 · 3 stories</div>" }) };
})()
```

Run the cell and read it back. Expected: one `text/html` output containing `engine ready` and no error.

- [ ] **Step 4: Insert the interactive storyboard cell**

Insert a JavaScript cell after `ENGINE_CELL_ID` with this source:

```javascript
await (async () => {
  const engine = globalThis.__spurProofSeries;
  if (!engine) throw new Error("Run the SPUR proof-film engine cell first");
  const targetPaths = {
    "control-loop": "/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/gallery-02-worker-visibility-1270x760.png",
    "durable-memory": "/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/gallery-05-session-resume-1270x760.png",
    "acp-agents": "/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/gallery-04-specialist-routing-1270x760.png"
  };
  const toBase64 = async (path) => {
    const bytes = await Deno.readFile(path);
    let binary = "";
    for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return btoa(binary);
  };
  const targetImages = Object.fromEntries(await Promise.all(Object.entries(targetPaths).map(async ([filmId, path]) => [filmId, await toBase64(path)])));
  const previews = Object.keys(engine.stories).map((filmId, index) => {
    const html = engine.buildFilmHtml({ filmId, capture: false });
    const encoded = btoa(unescape(encodeURIComponent(html)));
    const story = engine.stories[filmId];
    return `<button data-film="${filmId}" class="tab${index === 0 ? " active" : ""}">${story.eyebrow}</button><template id="tpl-${filmId}">${encoded}</template><template id="target-${filmId}">${targetImages[filmId]}</template>`;
  }).join("");
  const first = btoa(unescape(encodeURIComponent(engine.buildFilmHtml({ filmId: "control-loop", capture: false }))));
  const html = `<!doctype html><html><head><meta charset="utf-8"><style>*{box-sizing:border-box}body{margin:0;background:#0B0E14;color:#E6E1CF;font-family:Inter,sans-serif}.app{padding:22px}.head{display:flex;justify-content:space-between;align-items:end;border-bottom:1px solid #2A2E38;padding-bottom:16px}.eyebrow{color:#7FB4CA;font:700 12px ui-monospace;letter-spacing:.08em}h1{margin:5px 0 0;font-size:34px}.tabs{display:flex;gap:8px;margin:18px 0}.tab{cursor:pointer;border:1px solid #2A2E38;background:#111620;color:#AEB6C5;padding:10px 12px;font:700 11px ui-monospace}.tab.active{color:#E6E1CF;border-color:#7FB4CA}.compare{display:grid;grid-template-columns:1.35fr 1fr;gap:12px}.pane{background:#05070A;border:1px solid #2A2E38;padding:8px}.pane b{display:block;color:#7FB4CA;font:700 10px ui-monospace;margin-bottom:8px}iframe,img{display:block;width:100%;aspect-ratio:16/9;object-fit:contain;border:0;background:#05070A}.meta{display:grid;grid-template-columns:repeat(5,1fr);gap:8px;margin-top:12px}.meta div{background:#111620;border:1px solid #2A2E38;padding:10px;font:650 11px ui-monospace;color:#AEB6C5}</style></head><body><main class="app"><div class="head"><div><div class="eyebrow">SPUR · CONCEPT PROOF SERIES</div><h1>Match-cut storyboards</h1></div><div class="eyebrow">3 FILMS · 40.000S EACH</div></div><div class="tabs">${previews}</div><section class="compare"><div class="pane"><b>ANIMATED MODEL</b><iframe id="preview" sandbox="allow-scripts" src="data:text/html;base64,${first}"></iframe></div><div class="pane"><b>REAL PROOF TARGET</b><img id="target" src="data:image/png;base64,${targetImages['control-loop']}" alt="real SPUR proof target"></div></section><div class="meta"><div>0-3s · HOOK</div><div>3-13s · CONCEPT</div><div>13-16s · MATCH</div><div>16-35s · PROOF</div><div>35-40s · END</div></div></main><script>document.querySelectorAll('.tab').forEach(button=>button.addEventListener('click',()=>{document.querySelectorAll('.tab').forEach(x=>x.classList.remove('active'));button.classList.add('active');document.querySelector('#preview').src='data:text/html;base64,'+document.querySelector('#tpl-'+button.dataset.film).content.textContent;document.querySelector('#target').src='data:image/png;base64,'+document.querySelector('#target-'+button.dataset.film).content.textContent}))</script></body></html>`;
  return { [Symbol.for("Jupyter.display")]: () => ({ "text/html": html }) };
})()
```

Run and read the cell. Expected: exactly one `text/html` output, three film buttons, one sandboxed preview iframe, and no remote resource URL in the rendered output.

- [ ] **Step 5: Rewrite the production capture cell as a selector**

Use `notebook_write_cell` on `spur-ad-capture`. Start with:

```javascript
await (async () => {
  const FILM_ID = "control-loop";
  const engine = globalThis.__spurProofSeries;
  if (!engine) throw new Error("Run the SPUR proof-film engine cell first");
  globalThis.__spurProofFilmSelection = FILM_ID;
  const html = engine.buildFilmHtml({ filmId: FILM_ID, capture: true });
  return { [Symbol.for("Jupyter.display")]: () => ({ "text/html": html }) };
})()
```

Keep the cell's existing DAG source `{ "kind": "canvas-capture", "port": "spur-ad-capture" }`. Run and read it. Expected: exactly one `text/html` output and a fresh `spur-ad-capture` port version after 16 seconds.

- [ ] **Step 6: Rewrite the render cell to follow the selected film**

Use `notebook_write_cell` on `spur-ad-render` with:

```javascript
await (async () => {
  const { callTool } = await import(`file://${Deno.cwd()}/sdk/call_tool.ts`);
  const filmId = globalThis.__spurProofFilmSelection;
  const outputs = {
    "control-loop": "/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/series/motion/spur-control-loop-concept-v3-16s.mp4",
    "durable-memory": "/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/series/motion/spur-durable-memory-concept-v3-16s.mp4",
    "acp-agents": "/Volumes/Projects/spur/.spur/worktrees/d4-live-hitl-capture/docs/product_launch/media_pack/ph_ready/series/motion/spur-acp-agents-concept-v3-16s.mp4"
  };
  if (!outputs[filmId]) throw new Error("Unknown selected film: " + filmId);
  const response = await callTool("html_video_render", { port_names: ["spur-ad-capture"], output_path: outputs[filmId], fps: 30, resolution: "1920x1080" });
  const text = typeof response === "string" ? response : JSON.stringify(response);
  if (text.startsWith("Error executing tool")) throw new Error(text);
  const html = `<div style="background:#0B0E14;color:#E6E1CF;padding:18px;font:650 13px ui-monospace"><b style="color:#7FB4CA">RENDERED</b> · ${filmId}<br>${outputs[filmId]}</div>`;
  return { [Symbol.for("Jupyter.display")]: () => ({ "text/html": html }) };
})()
```

Do not run the render cell until Task 4.

- [ ] **Step 7: Run notebook conformance checks**

Call:

```text
notebook_dag_status()
notebook_app_doctor(path="/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb", level="static")
notebook_app_doctor(path="/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb", level="full")
```

Expected: no failed cells, the `spur-ad-capture` port is valid, and both doctors return `ok: true`. Legacy manifest warnings are acceptable; capability, plugin-spawn, skill, or port failures are not.

### Task 4: Render and verify the control-loop concept plate

**Files:**
- Modify through Notebook MCP only: `/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb`
- Create: `docs/product_launch/media_pack/ph_ready/series/motion/spur-control-loop-concept-v3-16s.mp4`

- [ ] **Step 1: Select and capture `control-loop`**

Confirm the selector line is `const FILM_ID = "control-loop";`, run the engine cell, run `spur-ad-capture`, wait for the capture port version to advance, then read the cell. Expected: one `text/html` output with no runtime error.

- [ ] **Step 2: Render the selected port**

Run `spur-ad-render`. Expected: the output cell reports `RENDERED · control-loop` and the versioned file exists.

- [ ] **Step 3: Verify exact media properties**

```bash
file="docs/product_launch/media_pack/ph_ready/series/motion/spur-control-loop-concept-v3-16s.mp4"
test "$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,avg_frame_rate,nb_frames -of csv=p=0 "$file")" = "h264,1920,1080,30/1,480"
test "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file")" = "16.000000"
ffmpeg -v error -i "$file" -f null -
```

Expected: all three commands exit 0.

- [ ] **Step 4: Review five temporal checkpoints**

Extract frames at 1.5s, 5s, 10s, 14.5s, and 15.9s. Confirm the hidden-work hook, four bounded workers, returned review loop, target TUI geometry, and readable copy.

### Task 5: Render and verify the durable-memory concept plate

**Files:**
- Modify through Notebook MCP only: `/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb`
- Create: `docs/product_launch/media_pack/ph_ready/series/motion/spur-durable-memory-concept-v3-16s.mp4`

- [ ] **Step 1: Select and capture `durable-memory`**

Use `notebook_edit_cell` to replace exactly `const FILM_ID = "control-loop";` with `const FILM_ID = "durable-memory";`. Run the engine and capture cells; wait for the capture port to advance.

- [ ] **Step 2: Render and verify**

Run `spur-ad-render`, then:

```bash
file="docs/product_launch/media_pack/ph_ready/series/motion/spur-durable-memory-concept-v3-16s.mp4"
test "$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,avg_frame_rate,nb_frames -of csv=p=0 "$file")" = "h264,1920,1080,30/1,480"
test "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file")" = "16.000000"
ffmpeg -v error -i "$file" -f null -
```

Expected: all checks exit 0.

- [ ] **Step 3: Review five temporal checkpoints**

Review 1.5s, 5s, 10s, 14.5s, and 15.9s. Confirm the agent process disappears, durable plan/lineage/evidence/review remain, resumed session reconnects, and final geometry matches Session Detail.

### Task 6: Render and verify the ACP-agent concept plate

**Files:**
- Modify through Notebook MCP only: `/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb`
- Create: `docs/product_launch/media_pack/ph_ready/series/motion/spur-acp-agents-concept-v3-16s.mp4`

- [ ] **Step 1: Select and capture `acp-agents`**

Use `notebook_edit_cell` to replace exactly `const FILM_ID = "durable-memory";` with `const FILM_ID = "acp-agents";`. Run the engine and capture cells; wait for the capture port to advance.

- [ ] **Step 2: Render and verify**

Run `spur-ad-render`, then:

```bash
file="docs/product_launch/media_pack/ph_ready/series/motion/spur-acp-agents-concept-v3-16s.mp4"
test "$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,avg_frame_rate,nb_frames -of csv=p=0 "$file")" = "h264,1920,1080,30/1,480"
test "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file")" = "16.000000"
ffmpeg -v error -i "$file" -f null -
```

Expected: all checks exit 0.

- [ ] **Step 3: Review five temporal checkpoints**

Review 1.5s, 5s, 10s, 14.5s, and 15.9s. Confirm four named agents, identical ACP ports, explicit `agent/model/effort`, one SPUR control spine, and routing-panel geometry.

- [ ] **Step 4: Rewrite the notebook preview artifact for all three plates**

Use `notebook_write_cell` on `spur-ad-video-embed`. Read the three MP4s, base64-encode them in 32KB chunks, and emit one self-contained `text/html` document with three `<video controls muted loop playsinline>` cards. Each card must show its title, 16.000s duration, target proof source, and takeaway. The rendered HTML must contain all three takeaways and no external resource URL.

- [ ] **Step 5: Commit the concept plates in the SPUR worktree**

```bash
git add docs/product_launch/media_pack/ph_ready/series/motion
git commit -m "feat(product-launch): D4.bi render concept proof motion plates"
```

### Task 7: Assemble three non-destructive Palmier proof-film timelines

**Files:**
- Create in Palmier project: `Proof Film 1 - Control Loop - 40s`
- Create in Palmier project: `Proof Film 2 - Durable Memory - 40s`
- Create in Palmier project: `Proof Film 3 - ACP Agents - 40s`

- [ ] **Step 1: Open and inspect the existing project once**

Call `manage_project(action="list")`, open `SPUR Product Hunt Hero - Real TUI` if it is not active, then call `get_media` and `get_timeline` once. Confirm the locked media IDs and that V2 timeline IDs `96C176C3` and `DC2C64B9` still exist.

- [ ] **Step 2: Import and inspect the three concept plates**

Call `import_media` for the three absolute worktree paths under folder `SPUR Product Hunt/Concept proof series/V3 motion`. Call `inspect_media(overview=true)` on every returned media ID. Expected: each is 1920x1080, 30fps, 16 seconds, and silent.

- [ ] **Step 3: Build Film 1 from returned boundaries**

Create an empty timeline named `Proof Film 1 - Control Loop - 40s`. Re-read it because timeline IDs and track IDs change. Then:

1. Add the control-loop concept media at `startFrame: 0`; use its returned end frame as the proof start.
2. Add `F2C142AD` with `source: [93, 112]` at that returned frame.
3. Use the diagnostic clip's returned end frame to add `5BECDF39`.
4. Add music `23DF6A98` with `source: [0, 40]` at frame 0 on an audio track.
5. Add `DRAFT · DIAGNOSTIC CAPTURE` over the diagnostic proof clip's exact returned frame pair.
6. Add `CLAUDE CODE · GROK · CODEX · OPENCODE` over the proof span at the bottom, using the existing V2 text style.
7. Reorder the base video track below text tracks by stable `trackId`.

Expected final base spans: `[0,480]`, `[480,1050]`, `[1050,1200]`; audio `[0,1200]`.

- [ ] **Step 4: Build Film 2 from returned boundaries**

Create `Proof Film 2 - Durable Memory - 40s`, re-read, then add:

1. Durable-memory concept at frame 0.
2. `791B452C` with `source: [8, 18]` at the concept's returned end.
3. `4B29113A` with `source: [0, 9]` at the prior clip's returned end.
4. `5BECDF39` at the resume clip's returned end.
5. Music `23DF6A98` with `source: [0, 40]` at frame 0.

Do not add a diagnostic watermark; both proof sources are approved. Expected final base spans: `[0,480]`, `[480,780]`, `[780,1050]`, `[1050,1200]`.

- [ ] **Step 5: Build Film 3 from returned boundaries**

Create `Proof Film 3 - ACP Agents - 40s`, re-read, then add:

1. ACP-agents concept at frame 0.
2. `63605F31` with `source: [49, 59]` at the concept's returned end.
3. `F2C142AD` with `source: [93, 102]` at the routing clip's returned end.
4. `5BECDF39` at the diagnostic clip's returned end.
5. Music `23DF6A98` with `source: [0, 40]` at frame 0.
6. `DRAFT · DIAGNOSTIC CAPTURE` over only the returned diagnostic clip frame pair.
7. `CLAUDE CODE · GROK · CODEX · OPENCODE` over the 19-second proof chapter.

Expected final base spans: `[0,480]`, `[480,780]`, `[780,1050]`, `[1050,1200]`.

- [ ] **Step 6: Inspect the three timelines at semantic boundaries**

For each timeline, call `inspect_timeline` at frames 45, 240, 435, 510, 900, and 1125. Confirm hook, concept, match, proof, diagnostic status, and end card. Re-read only after switching timelines or a failed mutation.

### Task 8: Export and independently verify the three films

**Files:**
- Create: `docs/product_launch/media_pack/ph_ready/series/spur-control-loop-proof-40s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/series/spur-durable-memory-proof-40s.mp4`
- Create: `docs/product_launch/media_pack/ph_ready/series/spur-acp-agents-proof-40s.mp4`

- [ ] **Step 1: Queue versioned exports**

Call `export_project` once per new timeline with `mode: "video"`, `codec: "H.264"`, `resolution: "Match Timeline"`, `overwrite: false`, and the exact absolute output paths from the manifest. Poll `manage_exports(action="list")` until all three jobs are `completed` or a concrete error appears.

- [ ] **Step 2: Run the fresh stream and decode gate**

```bash
for file in \
  docs/product_launch/media_pack/ph_ready/series/spur-control-loop-proof-40s.mp4 \
  docs/product_launch/media_pack/ph_ready/series/spur-durable-memory-proof-40s.mp4 \
  docs/product_launch/media_pack/ph_ready/series/spur-acp-agents-proof-40s.mp4; do
  test "$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,avg_frame_rate,nb_frames -of csv=p=0 "$file")" = "h264,1920,1080,30/1,1200"
  test "$(ffprobe -v error -select_streams a:0 -show_entries stream=codec_name,sample_rate,channels -of csv=p=0 "$file")" = "aac,48000,2"
  test "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file")" = "40.000000"
  ffmpeg -v error -i "$file" -f null -
  shasum -a 256 "$file"
done
```

Expected: every assertion exits 0, all full decodes are silent, and three hashes print.

- [ ] **Step 3: Create and review contact sheets**

For each film, extract frames at 1.5s, 6s, 11s, 14.5s, 20s, 33s, and 38s into a 7-column contact sheet. Review with `view_image`. Confirm readable motion, honest match cut, real proof visibility, correctly scoped watermark, and domain-free end card.

### Task 9: Record the final series in the media-pack notebook

**Files:**
- Modify through Notebook MCP only: `docs/product_launch/media_pack/product-hunt-media-pack.ipynb`
- Test: `docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh`

- [ ] **Step 1: Open and orient to the media-pack notebook**

Call `notebook_open` with the media-pack notebook path, then `notebook_context_pack` and `notebook_snapshot`. Do not edit the `.ipynb` with filesystem tools.

- [ ] **Step 2: Append the series delivery artifact**

Insert one Python code cell after the existing Palmier V2 delivery record. The cell must emit a `text/html` panel containing:

- Three film cards with exact filenames and 40.000s duration.
- The three Palmier timeline IDs returned in Task 7.
- The three SHA256 values from Task 8.
- Takeaways for all three films.
- Source status per proof segment.
- `DRAFT · DIAGNOSTIC CAPTURE` on Films 1 and 3; Film 2 marked approved-source-only.
- `INSTALL SPUR · COMMUNITY FREE` and `DOMAIN: NONE`.

Run and read the cell. Expected: one output containing `text/html`; no unrelated domain string.

- [ ] **Step 3: Return the notebook daemon to the html-video app**

Call `notebook_open(path="/Volumes/Projects/spur-notebook/app_gallery/html_video/app.ipynb")` and `notebook_context_pack()`. This flushes the media-pack notebook and restores the user's open motion app.

- [ ] **Step 4: Run both media contracts**

```bash
bash docs/product_launch/media_pack/tests/media-contract.test.sh
bash docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh
git diff --check
```

Expected: all existing media-pack contracts pass, all concept-proof series contracts pass, and `git diff --check` exits 0.

- [ ] **Step 5: Commit the final series**

```bash
git add \
  docs/product_launch/media_pack/concept-proof-series-manifest.json \
  docs/product_launch/media_pack/tests/concept-proof-series-contract.test.sh \
  docs/product_launch/media_pack/product-hunt-media-pack.ipynb \
  docs/product_launch/media_pack/ph_ready/series
git commit -m "feat(product-launch): D4.bj deliver concept proof film series"
```

- [ ] **Step 6: Verify the final worktree state**

```bash
git status --short
git log -5 --oneline
```

Expected: no status lines and the contract, manifest, motion-plate, and final-series commits are present.
