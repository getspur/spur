# spur-graph Four-Beat Explainer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build, review, render, and assemble the approved 48-second `spur-graph` explainer with a deterministic HyperFrames motion plate and Palmier-owned open captions.

**Architecture:** A flat HyperFrames composition owns the upper 896-pixel visual plate and uses one paused GSAP timeline across four consecutive clips. Data files hold the approved shot timing, captions, source evidence, and ownership boundary. HyperFrames produces a caption-free, audio-free plate; PalmierPro adds the lower caption band, silent AAC track, and final export.

**Tech Stack:** HyperFrames CLI, HTML/CSS, GSAP 3.14.2, Node.js 22+, FFmpeg/ffprobe, SPUR notebook MCP, PalmierPro

---

## Approved inputs

- Design: `docs/superpowers/specs/2026-07-27-spur-graph-explainer-design.md`
- Gate 2 notebook: `/Users/kevintruong/.spur/scratch/Untitled28.ipynb`
- Source revision for claims: `1dac840f2a5ebc81ad0862d4fe7dafcbf9c043e7`
- Runtime: 48.000 seconds, 1440 frames, 30 fps
- Canvas: 1920 by 1080
- Motion plate: top 896 pixels
- Palmier caption band: bottom 184 pixels
- Final hold: 45.5 to 48.0 seconds
- Paid generation: none

## File structure

| File | Responsibility |
|---|---|
| `videos/spur-graph-explainer/index.html` | HyperFrames composition DOM and semantic scene structure |
| `videos/spur-graph-explainer/composition.css` | Graph Utility palette, typography, grid, and scene layout |
| `videos/spur-graph-explainer/timeline.js` | One paused deterministic GSAP timeline |
| `videos/spur-graph-explainer/index.motion.json` | Seek-time motion assertions |
| `videos/spur-graph-explainer/captions.json` | Ten approved Palmier caption cues |
| `videos/spur-graph-explainer/shot-plan.json` | Twelve approved shot windows |
| `videos/spur-graph-explainer/source-manifest.json` | Code-grounded claims and rights |
| `videos/spur-graph-explainer/ownership.json` | Final-pixel ownership boundary |
| `videos/spur-graph-explainer/scripts/validate-contract.mjs` | Static duration, copy, ownership, and determinism checks |
| `videos/spur-graph-explainer/BRIEF.md` | Human-readable production lock |
| `videos/spur-graph-explainer/STORYBOARD.md` | Studio storyboard source |
| `videos/spur-graph-explainer/renders/spur-graph-explainer-plate.mp4` | Approved caption-free HyperFrames render |
| `deliveries/spur-graph-explainer/spur-graph-explainer-v1.mp4` | Palmier final export |
| `deliveries/spur-graph-explainer/manifest.json` | Final source and ownership manifest |
| `deliveries/spur-graph-explainer/validation.json` | ffprobe and visual-validation results |
| `deliveries/spur-graph-explainer/contact-sheet.jpg` | Final manual-review contact sheet |

### Task 1: Scaffold the HyperFrames project

**Files:**
- Create: `videos/spur-graph-explainer/`
- Verify: `videos/spur-graph-explainer/package.json`
- Verify: `videos/spur-graph-explainer/hyperframes.json`
- Verify: `videos/spur-graph-explainer/meta.json`

- [ ] **Step 1: Verify runtime prerequisites**

Run:

```bash
node --version
ffmpeg -version
ffprobe -version
```

Expected: Node reports version 22 or newer; FFmpeg and ffprobe exit zero.

- [ ] **Step 2: Scaffold from the Swiss Grid template**

Run from the repository root:

```bash
HYPERFRAMES_SKIP_SKILLS=1 npx hyperframes init videos/spur-graph-explainer \
  --non-interactive \
  --example swiss-grid \
  --resolution landscape
```

Expected: the new directory contains `index.html`, `package.json`,
`hyperframes.json`, `meta.json`, `AGENTS.md`, and `CLAUDE.md`.

- [ ] **Step 3: Confirm the generated project is isolated**

Run:

```bash
git status --short -- videos/spur-graph-explainer
```

Expected: only files under `videos/spur-graph-explainer/` are listed.

- [ ] **Step 4: Commit the scaffold**

```bash
git add videos/spur-graph-explainer
git commit -m "chore(video): G2 scaffold spur-graph explainer"
```

### Task 2: Add the production contracts and a failing validator

**Files:**
- Create: `videos/spur-graph-explainer/captions.json`
- Create: `videos/spur-graph-explainer/shot-plan.json`
- Create: `videos/spur-graph-explainer/source-manifest.json`
- Create: `videos/spur-graph-explainer/ownership.json`
- Create: `videos/spur-graph-explainer/scripts/validate-contract.mjs`
- Modify: `videos/spur-graph-explainer/package.json`

- [ ] **Step 1: Create the approved caption contract**

Write `captions.json` exactly as:

```json
{
  "owner": "palmier",
  "band": { "x": 0, "y": 896, "width": 1920, "height": 184 },
  "font": "IBM Plex Mono",
  "maxLines": 2,
  "cues": [
    { "id": "C01", "start": 0.7, "end": 4.7, "text": "A worktree holds code." },
    { "id": "C02", "start": 4.7, "end": 9.6, "text": "It does not answer graph questions." },
    { "id": "C03", "start": 10.4, "end": 15.0, "text": "spur-graph parses source with tree-sitter." },
    { "id": "C04", "start": 15.0, "end": 21.6, "text": "Fifteen languages become one GraphFacts layer." },
    { "id": "C05", "start": 22.4, "end": 27.4, "text": "Stable IDs preserve symbol identity." },
    { "id": "C06", "start": 27.4, "end": 31.6, "text": "Changed paths rebuild. Unchanged buckets are reused." },
    { "id": "C07", "start": 31.6, "end": 35.6, "text": "A BLAKE3 content hash records freshness." },
    { "id": "C08", "start": 36.4, "end": 40.4, "text": "code_* tools resolve symbols and trace callers." },
    { "id": "C09", "start": 40.4, "end": 44.8, "text": "Every response can carry freshness metadata." },
    { "id": "C10", "start": 44.8, "end": 48.0, "text": "Files become facts. Facts become trustworthy answers." }
  ]
}
```

The approved Gate 2 table contains the ten concrete cues above. The validator
and Palmier timeline must use all ten.

- [ ] **Step 2: Create the approved shot plan**

Write `shot-plan.json` exactly as:

```json
{
  "duration": 48,
  "fps": 30,
  "canvas": { "width": 1920, "height": 1080 },
  "shots": [
    { "id": "S01", "beat": "problem", "start": 0.0, "end": 0.7, "name": "Repository point" },
    { "id": "S02", "beat": "problem", "start": 0.7, "end": 2.2, "name": "File scatter" },
    { "id": "S03", "beat": "problem", "start": 2.2, "end": 4.8, "name": "Unanswered questions" },
    { "id": "S04", "beat": "problem", "start": 4.8, "end": 10.0, "name": "Problem lock" },
    { "id": "S05", "beat": "parse", "start": 10.0, "end": 12.8, "name": "Language support" },
    { "id": "S06", "beat": "parse", "start": 12.8, "end": 19.0, "name": "Extract facts" },
    { "id": "S07", "beat": "parse", "start": 19.0, "end": 22.0, "name": "GraphFacts lock" },
    { "id": "S08", "beat": "stabilize", "start": 22.0, "end": 26.0, "name": "Pin identity" },
    { "id": "S09", "beat": "stabilize", "start": 26.0, "end": 32.0, "name": "Incremental lanes" },
    { "id": "S10", "beat": "stabilize", "start": 32.0, "end": 36.0, "name": "Freshness rail" },
    { "id": "S11", "beat": "query", "start": 36.0, "end": 44.0, "name": "Query sequence" },
    { "id": "S12", "beat": "query", "start": 44.0, "end": 48.0, "name": "Answer and hold" }
  ]
}
```

- [ ] **Step 3: Create the source and ownership manifests**

Write `source-manifest.json` exactly as:

```json
{
  "revision": "1dac840f2a5ebc81ad0862d4fe7dafcbf9c043e7",
  "rights": "owned-repository",
  "sources": [
    { "beat": "problem", "path": "crates/spur-graph/ARCHITECTURE.md", "symbol": "architecture overview" },
    { "beat": "parse", "path": "crates/spur-graph/src/extract/languages.rs", "symbol": "Language", "graphId": "586e4ac59205a5f2" },
    { "beat": "parse", "path": "crates/spur-graph/src/extract/tree_sitter.rs", "symbol": "build_facts", "graphId": "8c7f88db29536dd2" },
    { "beat": "stabilize", "path": "crates/spur-graph/src/identity.rs", "symbol": "stable_symbol_id_for", "graphId": "56826c8f1b997fa1" },
    { "beat": "stabilize", "path": "crates/spur-graph/src/store/build.rs", "symbol": "artifact_from_facts_incremental", "graphId": "50e0a1ed10ab6434" },
    { "beat": "stabilize", "path": "crates/spur-graph/src/content_hash.rs", "symbol": "compute_graph_content_hash", "graphId": "4eeacbe2947eed97" },
    { "beat": "query", "path": "crates/spur-graph/src/schema.rs", "symbol": "GraphIndexArtifact", "graphId": "1caee5c63a6bdcbc" },
    { "beat": "query", "path": "crates/spur-graph/src/mcp/mod.rs", "symbol": "GraphMcpModule::dispatch", "graphId": "564787a2c4ce1ca3" },
    { "beat": "query", "path": "crates/spur-graph/src/mcp/mod.rs", "symbol": "GraphResponseMetadata", "graphId": "a65d3efc060d0f7c" }
  ]
}
```

Write `ownership.json` exactly as:

```json
{
  "visualPlate": {
    "owner": "html-video",
    "implementation": "hyperframes",
    "region": { "x": 0, "y": 0, "width": 1920, "height": 896 },
    "scenes": ["problem", "parse", "stabilize", "query"]
  },
  "captions": {
    "owner": "palmier",
    "region": { "x": 0, "y": 896, "width": 1920, "height": 184 }
  },
  "audio": { "owner": "palmier", "kind": "silent-aac" },
  "finalAssembly": { "owner": "palmier" }
}
```

- [ ] **Step 4: Write the structural validator**

Write `scripts/validate-contract.mjs` exactly as:

```javascript
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const readText = (path) => readFile(new URL(`../${path}`, import.meta.url), "utf8");
const readJson = async (path) => JSON.parse(await readText(path));

const [html, timeline, captions, shots, sources, ownership] = await Promise.all([
  readText("index.html"),
  readText("timeline.js"),
  readJson("captions.json"),
  readJson("shot-plan.json"),
  readJson("source-manifest.json"),
  readJson("ownership.json")
]);

assert.match(html, /data-duration="48"/);
assert.match(html, /data-fps="30"/);
assert.match(html, /data-width="1920"/);
assert.match(html, /data-height="1080"/);
assert.match(html, /data-start="0"[\s\S]*data-duration="10"/);
assert.match(html, /data-start="10"[\s\S]*data-duration="12"/);
assert.match(html, /data-start="22"[\s\S]*data-duration="14"/);
assert.match(html, /data-start="36"[\s\S]*data-duration="12"/);

assert.equal(captions.owner, "palmier");
assert.equal(captions.cues.length, 10);
assert.equal(captions.cues.at(-1).end, 48);
for (const cue of captions.cues) {
  assert.ok(cue.start < cue.end, `${cue.id} has a non-positive duration`);
  assert.equal(html.includes(cue.text), false, `${cue.id} was baked into the plate`);
}

assert.equal(shots.duration, 48);
assert.equal(shots.fps, 30);
assert.equal(shots.shots.length, 12);
assert.equal(shots.shots[0].start, 0);
assert.equal(shots.shots.at(-1).end, 48);
for (let index = 1; index < shots.shots.length; index += 1) {
  assert.equal(shots.shots[index - 1].end, shots.shots[index].start);
}

assert.equal(sources.revision, "1dac840f2a5ebc81ad0862d4fe7dafcbf9c043e7");
assert.equal(sources.sources.length, 9);
assert.equal(ownership.visualPlate.owner, "html-video");
assert.equal(ownership.visualPlate.region.height, 896);
assert.equal(ownership.captions.owner, "palmier");
assert.equal(ownership.captions.region.y, 896);
assert.equal(ownership.captions.region.height, 184);
assert.equal(ownership.finalAssembly.owner, "palmier");

assert.match(timeline, /gsap\.timeline\(\{ paused: true \}\)/);
assert.match(timeline, /window\.__timelines\["spur-graph-explainer"\] = tl/);
assert.match(timeline, /const FINAL_HOLD_START = 45\.5/);
assert.doesNotMatch(timeline, /Math\.random|Date\.now|repeat:\s*-1/);
assert.doesNotMatch(html, /@keyframes/);

console.log("spur-graph explainer contract: ok");
```

- [ ] **Step 5: Add the validator script to `package.json`**

Add:

```json
{
  "scripts": {
    "verify-contract": "node scripts/validate-contract.mjs"
  }
}
```

Preserve the scaffolded pinned `dev`, `check`, `render`, and `publish` scripts.

- [ ] **Step 6: Run the validator and confirm the expected failure**

Run:

```bash
npm run verify-contract
```

Expected: FAIL because the scaffold does not yet have the approved 48-second
composition or `timeline.js`.

- [ ] **Step 7: Commit the failing contract test**

```bash
git add videos/spur-graph-explainer
git commit -m "test(video): G2 define explainer production contract"
```

### Task 3: Author the four-scene composition DOM and Graph Utility styles

**Files:**
- Modify: `videos/spur-graph-explainer/index.html`
- Create: `videos/spur-graph-explainer/composition.css`
- Create: `videos/spur-graph-explainer/BRIEF.md`
- Create: `videos/spur-graph-explainer/STORYBOARD.md`

- [ ] **Step 1: Replace `index.html` with the exact scene structure**

Use this complete document:

```html
<!doctype html>
<html lang="en" data-resolution="landscape">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=1920, height=1080" />
    <title>spur-graph: Files to Trustworthy Answers</title>
    <link rel="stylesheet" href="./composition.css" />
    <script src="https://cdn.jsdelivr.net/npm/gsap@3.14.2/dist/gsap.min.js"></script>
  </head>
  <body>
    <main id="stage" data-composition-id="spur-graph-explainer" data-start="0"
      data-duration="48" data-fps="30" data-width="1920" data-height="1080">
      <div class="grid" data-layout-ignore aria-hidden="true"></div>

      <section id="beat-problem" class="clip beat" data-start="0"
        data-duration="10" data-track-index="1">
        <h1 id="problem-headline" class="headline">A worktree is not a queryable graph</h1>
        <div id="repo-point" class="repo-point">WORKTREE</div>
        <div class="file-card" id="file-identity">identity.rs</div>
        <div class="file-card" id="file-schema">schema.rs</div>
        <div class="file-card" id="file-tree">tree_sitter.rs</div>
        <div class="file-card" id="file-build">build.rs</div>
        <div class="file-card" id="file-mcp">mcp/mod.rs</div>
        <div class="question" id="question-where">WHERE IS X?</div>
        <div class="question" id="question-callers">WHO CALLS IT?</div>
        <div class="question problem" id="question-fresh">IS IT FRESH?</div>
      </section>

      <section id="beat-parse" class="clip beat" data-start="10"
        data-duration="12" data-track-index="1">
        <h1 id="parse-headline" class="headline">Tree-sitter normalizes the worktree</h1>
        <div id="language-cluster" class="language-cluster">
          <span>Rust</span><span>TypeScript</span><span>Python</span><span>Go</span>
          <span>Java</span><span>C</span><span>C++</span><span>+8</span>
        </div>
        <div id="build-facts" class="engine-card">build_facts</div>
        <div id="graph-facts" class="facts-card">GraphFacts</div>
        <svg class="parse-routes" viewBox="0 0 1920 896" aria-hidden="true">
          <path id="parse-in" d="M 480 470 H 790" />
          <path id="parse-out" d="M 1110 470 H 1410" />
        </svg>
      </section>

      <section id="beat-stabilize" class="clip beat" data-start="22"
        data-duration="14" data-track-index="1">
        <h1 id="stabilize-headline" class="headline">Stable IDs keep the graph trustworthy</h1>
        <article id="identity-card" class="identity-card">
          <strong>stable_symbol_id_for</strong>
          <span>path + kind + owner + name</span>
        </article>
        <div id="incremental-group" class="incremental-group">
          <div id="changed-lane" class="lane"><b>CHANGED</b><span>RE-EXTRACT BUCKET</span></div>
          <div id="reused-lane" class="lane reused"><b>UNCHANGED</b><span>REUSE BUCKET</span></div>
          <div class="hash-label">BLAKE3 CONTENT HASH</div>
          <div id="hash-rail" class="hash-rail"></div>
        </div>
      </section>

      <section id="beat-query" class="clip beat" data-start="36"
        data-duration="12" data-track-index="1">
        <h1 id="query-headline" class="headline">Ask the graph, then verify freshness</h1>
        <div id="query-stack" class="query-stack">
          <div id="query-resolve" class="query-state">code_resolve</div>
          <div id="query-read" class="query-state">code_read_symbol</div>
          <div id="query-callers" class="query-state">code_callers</div>
          <div id="query-callees" class="query-state">code_callees</div>
        </div>
        <article id="answer-card" class="answer-card">
          <strong>TRUST THE ANSWER</strong>
          <span>files + symbols + edges</span>
          <span>history + tombstones</span>
          <span>freshness metadata</span>
        </article>
        <div id="mnemonic" class="mnemonic">
          <span>PROBLEM</span><i></i><span>PARSE</span><i></i>
          <span>STABILIZE</span><i></i><span>QUERY</span>
        </div>
      </section>

      <div id="caption-reserve" data-layout-ignore aria-hidden="true"></div>
    </main>
    <script src="./timeline.js"></script>
  </body>
</html>
```

- [ ] **Step 2: Create `composition.css`**

Use this complete stylesheet:

```css
:root {
  --paper: #f8faf8;
  --ink: #17201d;
  --green: #38a969;
  --pale: #dff4e6;
  --grid: #dfe9e2;
  --muted: #607068;
  --problem: #b84231;
}

* { box-sizing: border-box; }
html, body {
  width: 1920px;
  height: 1080px;
  margin: 0;
  overflow: hidden;
  background: var(--paper);
  color: var(--ink);
  font-family: "IBM Plex Mono", ui-monospace, monospace;
}
#stage { position: relative; width: 1920px; height: 1080px; overflow: hidden; }
.grid {
  position: absolute; inset: 0;
  background-color: var(--paper);
  background-image:
    linear-gradient(var(--grid) 1px, transparent 1px),
    linear-gradient(90deg, var(--grid) 1px, transparent 1px);
  background-size: 48px 48px;
}
.beat { position: absolute; inset: 0 0 184px; overflow: hidden; }
.headline {
  position: absolute; top: 70px; left: 96px; right: 96px; margin: 0;
  font-family: Inter, Arial, sans-serif;
  font-size: 72px; font-weight: 900; line-height: 0.98;
  letter-spacing: -0.055em; text-transform: uppercase;
}
#caption-reserve { position: absolute; left: 0; right: 0; bottom: 0; height: 184px; }
.repo-point, .file-card, .question, .engine-card, .facts-card,
.identity-card, .lane, .query-state, .answer-card {
  border: 3px solid var(--ink); background: white;
}
.repo-point {
  position: absolute; left: 810px; top: 400px; width: 300px;
  padding: 30px; text-align: center; font-size: 28px; font-weight: 800;
}
.file-card {
  position: absolute; width: 280px; padding: 24px 20px;
  text-align: center; font-size: 23px; font-weight: 800;
  box-shadow: 8px 8px 0 #cde7d5;
}
#file-identity { left: 120px; top: 310px; }
#file-schema { left: 500px; top: 520px; }
#file-tree { left: 820px; top: 300px; }
#file-build { left: 1200px; top: 530px; }
#file-mcp { left: 1500px; top: 320px; }
.question {
  position: absolute; padding: 18px 22px; background: var(--ink);
  color: white; font-size: 23px; font-weight: 800;
}
#question-where { left: 170px; top: 700px; }
#question-callers { right: 170px; top: 680px; }
#question-fresh { left: 810px; top: 690px; background: var(--problem); }
.language-cluster {
  position: absolute; left: 110px; top: 300px; width: 520px;
  display: flex; flex-wrap: wrap; gap: 14px;
}
.language-cluster span {
  padding: 16px 18px; border: 2px solid #9ab8a4; background: white;
  font-size: 22px; font-weight: 800;
}
.engine-card, .facts-card {
  position: absolute; top: 390px; display: grid; place-items: center;
  height: 160px; font-size: 31px; font-weight: 900;
}
.engine-card { left: 790px; width: 320px; background: var(--pale); }
.facts-card { right: 110px; width: 400px; border-color: var(--green); box-shadow: 10px 10px 0 #bfe7cc; }
.parse-routes { position: absolute; inset: 0; width: 1920px; height: 896px; }
.parse-routes path {
  fill: none; stroke: var(--green); stroke-width: 8;
  stroke-linecap: square; stroke-linejoin: round;
}
.identity-card {
  position: absolute; left: 120px; top: 330px; width: 560px;
  padding: 34px; background: var(--ink); color: white;
}
.identity-card strong { display: block; margin-bottom: 18px; color: #72de96; font-size: 30px; }
.identity-card span { font-size: 23px; }
.incremental-group { position: absolute; left: 920px; top: 310px; width: 760px; }
.lane { display: grid; grid-template-columns: 230px 1fr; min-height: 96px; margin-bottom: 22px; }
.lane b { display: grid; place-items: center; background: #278b55; color: white; font-size: 23px; }
.lane span { display: grid; place-items: center; font-size: 22px; font-weight: 800; }
.lane.reused b { background: var(--muted); }
.hash-label { margin-top: 38px; color: var(--muted); font-size: 18px; font-weight: 800; letter-spacing: 0.08em; }
.hash-rail { width: 100%; height: 16px; margin-top: 12px; background: var(--green); transform-origin: left center; }
.query-stack { position: absolute; left: 120px; top: 300px; width: 650px; height: 160px; }
.query-state {
  position: absolute; inset: 0; display: grid; place-items: center;
  border-left: 14px solid var(--green); font-size: 38px; font-weight: 900;
}
.answer-card {
  position: absolute; right: 120px; top: 280px; width: 760px; padding: 38px;
  box-shadow: 12px 12px 0 #c8e7d2;
}
.answer-card strong {
  display: block; margin-bottom: 26px; font-family: Inter, Arial, sans-serif;
  font-size: 54px; font-weight: 900;
}
.answer-card span { display: block; margin: 13px 0; color: #236b43; font-size: 25px; font-weight: 800; }
.mnemonic {
  position: absolute; left: 120px; right: 120px; top: 730px;
  display: flex; align-items: center; justify-content: space-between;
  font-size: 23px; font-weight: 900; letter-spacing: 0.08em;
}
.mnemonic i { flex: 1; height: 6px; margin: 0 24px; background: var(--green); }
```

- [ ] **Step 3: Add the human-readable brief and storyboard**

Write `BRIEF.md` exactly as:

```markdown
# spur-graph Explainer Brief

- Audience: SPUR engineers and contributors
- Purpose: contributor orientation
- Runtime: 48.000 seconds at 30 fps
- Canvas: 1920 by 1080
- Direction: Graph Utility
- Delivery: silent, text-led, with Palmier open captions
- Source revision: 1dac840f2a5ebc81ad0862d4fe7dafcbf9c043e7
- Visual owner: HTML Video, implemented with HyperFrames, top 896 pixels
- Caption, audio, and final assembly owner: Palmier, bottom 184 pixels
- Paid generation: none

Takeaway: Files become facts. Facts become trustworthy answers.
```

Write `STORYBOARD.md` exactly as:

```markdown
# spur-graph Explainer Storyboard

| Shot | Window | Beat | Picture | Captions |
|---|---:|---|---|---|
| S01 | 00:00.00-00:00.70 | Problem | Repository point | none |
| S02 | 00:00.70-00:02.20 | Problem | File scatter | C01 |
| S03 | 00:02.20-00:04.80 | Problem | Unanswered questions | C01 |
| S04 | 00:04.80-00:10.00 | Problem | Problem lock | C02 |
| S05 | 00:10.00-00:12.80 | Parse | Language support | C03 |
| S06 | 00:12.80-00:19.00 | Parse | Extract facts | C03, C04 |
| S07 | 00:19.00-00:22.00 | Parse | GraphFacts lock | C04 |
| S08 | 00:22.00-00:26.00 | Stabilize | Pin identity | C05 |
| S09 | 00:26.00-00:32.00 | Stabilize | Incremental lanes | C05, C06, C07 |
| S10 | 00:32.00-00:36.00 | Stabilize | Freshness rail | C07 |
| S11 | 00:36.00-00:44.00 | Query | Query sequence | C08, C09 |
| S12 | 00:44.00-00:48.00 | Query | Answer and hold | C09, C10 |
```

- [ ] **Step 4: Run the fast static linter**

Run:

```bash
npx hyperframes lint --json
```

Expected: no missing composition, clip, timing, track, or timeline-registration
errors. A missing `timeline.js` error is acceptable until Task 4; all other
errors must be fixed immediately.

- [ ] **Step 5: Commit the composition structure**

```bash
git add videos/spur-graph-explainer
git commit -m "feat(video): G2 lay out spur-graph story"
```

### Task 4: Implement the seek-safe HyperFrames timeline

**Files:**
- Create: `videos/spur-graph-explainer/timeline.js`
- Create: `videos/spur-graph-explainer/index.motion.json`

- [ ] **Step 1: Create `timeline.js`**

Use this complete timeline:

```javascript
window.__timelines = window.__timelines || {};
const tl = gsap.timeline({ paused: true });
const FINAL_HOLD_START = 45.5;

const hideAtStart = (selector) => {
  tl.set(selector, { opacity: 0 }, 0);
};

[
  "#problem-headline", "#question-where", "#question-callers", "#question-fresh",
  "#parse-headline", "#language-cluster span", "#build-facts", "#graph-facts",
  "#stabilize-headline", "#identity-card", "#changed-lane", "#reused-lane",
  "#query-headline", "#answer-card", "#mnemonic", ".query-state"
].forEach(hideAtStart);

const fileStarts = [
  ["#file-identity", 690, 150],
  ["#file-schema", 310, -60],
  ["#file-tree", -10, 160],
  ["#file-build", -390, -70],
  ["#file-mcp", -690, 140]
];

tl.fromTo("#repo-point",
  { opacity: 0, scale: 0.82 },
  { opacity: 1, scale: 1, duration: 0.5, ease: "power2.out" },
  0.1
);

fileStarts.forEach(([selector, x, y], index) => {
  tl.fromTo(selector,
    { x, y, scale: 0.6, opacity: 0 },
    { x: 0, y: 0, scale: 1, opacity: 1, duration: 1.35, ease: "power3.out" },
    0.7 + index * 0.06
  );
});
tl.to("#repo-point", { opacity: 0, duration: 0.25, ease: "power1.out" }, 1.85);
tl.fromTo("#problem-headline",
  { opacity: 0, y: -24 },
  { opacity: 1, y: 0, duration: 0.55, ease: "power3.out" },
  2.15
);
tl.fromTo(["#question-where", "#question-callers", "#question-fresh"],
  { opacity: 0, y: 18 },
  { opacity: 1, y: 0, duration: 0.42, ease: "power3.out", stagger: 0.14 },
  2.3
);
tl.to(["#file-identity", "#file-schema", "#file-build", "#file-mcp",
  "#question-where", "#question-callers", "#question-fresh"],
  { opacity: 0, duration: 0.5, ease: "power2.in" },
  8.85
);
tl.to("#file-tree", { x: -670, y: 10, scale: 0.9, duration: 0.7, ease: "power3.inOut" }, 9.0);

["#parse-in", "#parse-out"].forEach((selector) => {
  const path = document.querySelector(selector);
  const length = path.getTotalLength();
  path.style.strokeDasharray = String(length);
  path.style.strokeDashoffset = String(length);
});
tl.fromTo("#parse-headline",
  { opacity: 0, y: -24 },
  { opacity: 1, y: 0, duration: 0.55, ease: "power3.out" },
  10.2
);
tl.fromTo("#language-cluster span",
  { opacity: 0, y: 18 },
  { opacity: 1, y: 0, duration: 0.42, ease: "power3.out", stagger: 0.08 },
  10.5
);
tl.fromTo("#build-facts",
  { opacity: 0, scale: 0.94 },
  { opacity: 1, scale: 1, duration: 0.5, ease: "power3.out" },
  12.55
);
tl.to("#parse-in", { strokeDashoffset: 0, duration: 0.55, ease: "power2.out" }, 12.8);
tl.to("#parse-out", { strokeDashoffset: 0, duration: 0.55, ease: "power2.out" }, 13.2125);
tl.fromTo("#graph-facts",
  { opacity: 0, x: 38 },
  { opacity: 1, x: 0, duration: 0.62, ease: "power3.out" },
  18.8
);

tl.fromTo("#stabilize-headline",
  { opacity: 0, y: -24 },
  { opacity: 1, y: 0, duration: 0.55, ease: "power3.out" },
  22.2
);
tl.fromTo("#identity-card",
  { opacity: 0, x: -40 },
  { opacity: 1, x: 0, duration: 0.7, ease: "power3.out" },
  22.4
);
tl.set("#incremental-group", { x: 0 }, 22);
tl.to("#incremental-group", { x: -32, duration: 0.14, ease: "power3.in" }, 26.0);
tl.to("#incremental-group", { x: -257, duration: 0.12, ease: "none" }, 26.14);
tl.to("#incremental-group", { x: -288, duration: 0.44, ease: "power4.out" }, 26.26);
tl.fromTo(["#changed-lane", "#reused-lane"],
  { opacity: 0 },
  { opacity: 1, duration: 0.12, ease: "none", stagger: 0.06 },
  26.14
);
tl.fromTo("#hash-rail",
  { scaleX: 0 },
  { scaleX: 1, duration: 0.85, ease: "power2.out" },
  32.0
);

tl.fromTo("#query-headline",
  { opacity: 0, y: -24 },
  { opacity: 1, y: 0, duration: 0.55, ease: "power3.out" },
  36.2
);
const queryWindows = [
  ["#query-resolve", 36.0, 38.0],
  ["#query-read", 38.0, 40.0],
  ["#query-callers", 40.0, 42.0],
  ["#query-callees", 42.0, 44.0]
];
queryWindows.forEach(([selector, start, end]) => {
  tl.fromTo(selector,
    { opacity: 0, scale: 0.96 },
    { opacity: 1, scale: 1, duration: 0.35, ease: "power3.out" },
    start
  );
  tl.to(selector, { opacity: 0, duration: 0.25, ease: "power2.in" }, end - 0.25);
});
tl.fromTo("#answer-card",
  { opacity: 0, x: 48 },
  { opacity: 1, x: 0, duration: 0.65, ease: "power3.out" },
  44.0
);
tl.fromTo("#mnemonic",
  { opacity: 0, y: 18 },
  { opacity: 1, y: 0, duration: 0.55, ease: "power3.out" },
  FINAL_HOLD_START
);

tl.seek(0);
window.__timelines["spur-graph-explainer"] = tl;
```

- [ ] **Step 2: Create the motion sidecar**

Write `index.motion.json` exactly as:

```json
{
  "duration": 48,
  "assertions": [
    { "kind": "appearsBy", "selector": "#problem-headline", "bySec": 3 },
    { "kind": "appearsBy", "selector": "#graph-facts", "bySec": 20 },
    { "kind": "appearsBy", "selector": "#identity-card", "bySec": 24 },
    { "kind": "appearsBy", "selector": "#answer-card", "bySec": 45 },
    { "kind": "before", "a": "#problem-headline", "b": "#parse-headline" },
    { "kind": "before", "a": "#parse-headline", "b": "#stabilize-headline" },
    { "kind": "before", "a": "#stabilize-headline", "b": "#query-headline" },
    { "kind": "staysInFrame", "selector": ".headline" },
    { "kind": "staysInFrame", "selector": ".file-card" },
    { "kind": "staysInFrame", "selector": "#answer-card" }
  ]
}
```

- [ ] **Step 3: Run the contract validator**

Run:

```bash
npm run verify-contract
```

Expected:

```text
spur-graph explainer contract: ok
```

- [ ] **Step 4: Run HyperFrames lint**

Run:

```bash
npx hyperframes lint --json
```

Expected: zero lint errors.

- [ ] **Step 5: Commit the deterministic timeline**

```bash
git add videos/spur-graph-explainer
git commit -m "feat(video): G2 animate spur-graph pipeline"
```

### Task 5: Run the browser gate and review snapshots

**Files:**
- Modify when findings require it: `videos/spur-graph-explainer/index.html`
- Modify when findings require it: `videos/spur-graph-explainer/composition.css`
- Modify when findings require it: `videos/spur-graph-explainer/timeline.js`
- Inspect: `videos/spur-graph-explainer/snapshots/`

- [ ] **Step 1: Run the required browser check**

Run:

```bash
npx hyperframes check \
  --json \
  --snapshots \
  --samples 17 \
  --at 0.7,2.2,4.8,10,12.8,19,22,26,32,36,40,44,45.5,47.9 \
  --at-transitions \
  --caption-zone "x0=0;y0=.8296;x1=1;y1=1;severity=error;seek=.05,.15,.27,.42,.58,.72,.86,.95"
```

Expected: `ok` is `true`, with zero runtime, layout, motion, contrast, and
caption-zone errors.

- [ ] **Step 2: Inspect the generated overview frames**

Open every generated overview image. Verify:

- Beat 1 file names are legible.
- Beat 2 visually reads left to right.
- Beat 3 clearly separates changed and unchanged lanes.
- Beat 4 query states do not overlap.
- No visual content enters the lower 184-pixel caption band.
- The frame at 47.9 seconds matches the 45.5-second mnemonic lock.

- [ ] **Step 3: Fix every persistent finding**

Use the finding selector, timestamp, and bounding box to edit only the implicated
file. Re-run the command from Step 1 after each fix batch.

- [ ] **Step 4: Commit the browser-green composition**

```bash
git add videos/spur-graph-explainer
git commit -m "fix(video): G2 clear explainer browser gate"
```

### Task 6: Open the Companion preview and wait for render approval

**Files:**
- No source changes required

- [ ] **Step 1: Start HyperFrames Studio in the background**

Run from `videos/spur-graph-explainer/`:

```bash
npx hyperframes preview --port 3017
```

Keep the process alive. Confirm the project URL responds:

```text
http://localhost:3017/#project/spur-graph-explainer
```

- [ ] **Step 2: Give the user the live Studio URL**

Report the exact URL above and ask for final composition preview approval.

- [ ] **Step 3: Stop before rendering**

Do not run a render command until the user explicitly approves this final
composition preview.

### Task 7: Render and verify the caption-free motion plate

**Files:**
- Create: `videos/spur-graph-explainer/renders/spur-graph-explainer-plate.mp4`
- Create: `videos/spur-graph-explainer/renders/plate-ffprobe.json`

- [ ] **Step 1: Render only after preview approval**

Run:

```bash
npx hyperframes render \
  --quality high \
  --strict \
  --fps 30 \
  --output renders/spur-graph-explainer-plate.mp4
```

Expected: the render exits zero and the output is non-empty.

- [ ] **Step 2: Record ffprobe evidence**

Run:

```bash
test -s renders/spur-graph-explainer-plate.mp4
ffprobe -v error -show_streams -show_format -of json \
  renders/spur-graph-explainer-plate.mp4 \
  > renders/plate-ffprobe.json
```

Expected:

- duration is 48 seconds within one frame;
- width is 1920;
- height is 1080;
- average frame rate is 30;
- video codec is H.264;
- no caption or audio stream is present.

- [ ] **Step 3: Submit one HyperFrames feedback report**

For a clean render:

```bash
npx hyperframes feedback --rating 10 --comment "48s deterministic plate rendered and passed seek, layout, motion, contrast, and caption-zone checks."
```

- [ ] **Step 4: Commit render evidence without committing the large MP4 if ignored**

```bash
git add videos/spur-graph-explainer/renders/plate-ffprobe.json
git commit -m "test(video): G2 verify explainer plate"
```

### Task 8: Assemble captions and silent audio in PalmierPro

**Files:**
- Create: `deliveries/spur-graph-explainer/spur-graph-explainer-v1.mp4`
- Create: `deliveries/spur-graph-explainer/manifest.json`

- [ ] **Step 1: Confirm a PalmierPro editing session is available**

Use the connected PalmierPro session. If no session or editing connector is
available, stop at this step and request that the user connect or open the
PalmierPro project. Do not substitute FFmpeg, another NLE, or baked HyperFrames
captions because Gate 2 assigns final assembly exclusively to Palmier.

- [ ] **Step 2: Create the final timeline**

Create a new timeline named:

```text
spur-graph - Files to Trustworthy Answers - 48s
```

Set it to 1920 by 1080, 30 fps, and 48 seconds. Import
`renders/spur-graph-explainer-plate.mp4` on the primary video track at 00:00.

- [ ] **Step 3: Add the ten open-caption cues**

Create one native Palmier caption item for each `captions.json` entry. Constrain
all captions to the bottom 184-pixel band, center-align them, use IBM Plex Mono,
and preserve the approved cue boundaries exactly.

- [ ] **Step 4: Add silent AAC audio**

Add a silent stereo audio item spanning 00:00 to 00:48. Do not add narration,
music, or sound effects.

- [ ] **Step 5: Export the final MP4**

Export H.264 video with AAC audio to:

```text
deliveries/spur-graph-explainer/spur-graph-explainer-v1.mp4
```

- [ ] **Step 6: Write the final manifest**

`manifest.json` must include:

```json
{
  "title": "spur-graph: Files to Trustworthy Answers",
  "duration": 48,
  "fps": 30,
  "canvas": { "width": 1920, "height": 1080 },
  "sourceRevision": "1dac840f2a5ebc81ad0862d4fe7dafcbf9c043e7",
  "visualOwner": "html-video",
  "visualImplementation": "hyperframes",
  "captionOwner": "palmier",
  "audioOwner": "palmier",
  "finalEditor": "palmier",
  "captionCueCount": 10,
  "paidGeneration": false
}
```

### Task 9: Validate delivery and add the notebook review artifact

**Files:**
- Create: `deliveries/spur-graph-explainer/validation.json`
- Create: `deliveries/spur-graph-explainer/contact-sheet.jpg`
- Modify through notebook MCP: `/Users/kevintruong/.spur/scratch/Untitled28.ipynb`

- [ ] **Step 1: Probe the final export**

Run:

```bash
ffprobe -v error -show_streams -show_format -of json \
  deliveries/spur-graph-explainer/spur-graph-explainer-v1.mp4 \
  > deliveries/spur-graph-explainer/validation.json
```

Verify H.264 video, AAC audio, 1920 by 1080, 30 fps, and 48 seconds within one
frame.

- [ ] **Step 2: Verify the audio is silent**

Run:

```bash
ffmpeg -i deliveries/spur-graph-explainer/spur-graph-explainer-v1.mp4 \
  -af volumedetect -f null - 2>&1
```

Expected: the detected maximum volume is silence or effectively silent.

- [ ] **Step 3: Generate a twelve-frame contact sheet**

Run:

```bash
ffmpeg -i deliveries/spur-graph-explainer/spur-graph-explainer-v1.mp4 \
  -vf "select='eq(n,10)+eq(n,44)+eq(n,105)+eq(n,222)+eq(n,342)+eq(n,477)+eq(n,615)+eq(n,720)+eq(n,870)+eq(n,1020)+eq(n,1200)+eq(n,1380)',scale=480:270,tile=4x3" \
  -frames:v 1 deliveries/spur-graph-explainer/contact-sheet.jpg
```

Inspect caption line breaks, bottom-band containment, graph continuity, and the
motionless final hold.

- [ ] **Step 4: Add the final notebook review cell**

Use notebook MCP only. Insert and run a self-contained JavaScript cell whose
output MIME is `text/html`. The output must show:

- final MP4 path and ffprobe summary;
- the twelve contact-sheet frames;
- ten caption cue windows;
- source revision and ownership summary;
- pass/fail badges for duration, fps, codec, audio, caption zone, final hold,
  and source manifest.

Re-read the cell and verify one `text/html` output.

- [ ] **Step 5: Commit the delivery metadata**

```bash
git add deliveries/spur-graph-explainer videos/spur-graph-explainer
git commit -m "docs(video): G2 record spur-graph delivery"
```

- [ ] **Step 6: Report the final handoff**

Provide clickable paths to the final MP4, Palmier project, notebook, manifest,
validation report, and contact sheet. State the exact validation result and any
remaining manual-review caveat.

## Self-review checklist

- [ ] Every Gate 1 design requirement maps to a task.
- [ ] Every Gate 2 cue, shot, source select, and ownership boundary is encoded.
- [ ] The HyperFrames plate contains no open-caption text.
- [ ] The final 2.5 seconds are intentionally static and allowed by motion checks.
- [ ] Rendering remains blocked on explicit Studio preview approval.
- [ ] Palmier remains the sole final editor.
- [ ] No paid-generation gate is required.
- [ ] Large rendered media follows the existing repository ignore policy.
