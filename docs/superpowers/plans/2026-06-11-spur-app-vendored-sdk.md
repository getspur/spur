# Spur App Vendored TypeScript SDK Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Spur apps hermetic by vendoring the TypeScript SDK modules an app uses into the app root, declared in `spur-app.json` and enforced by `notebook_app_doctor`, so notebook cells import `file://${Deno.cwd()}/sdk/…` instead of reaching outside the app with `../../sdk/typescript/src/…`.

**Architecture:** Three layers. (1) Manifest: a new optional `sdk` block in `spur-app.json` (`SpurAppManifest` in `crates/spur-notebook/src/spur_app.rs`) declares the vendored SDK directory. (2) Doctor: `notebook_app_doctor` gains a check that a declared SDK dir exists and contains the required modules. (3) App: `app_gallery/html_video` gets a `sdk/` directory containing byte-identical copies of `sdk/typescript/src/call_tool.ts` and `wire.ts`, the notebook imports switch to `file://${Deno.cwd()}/sdk/call_tool.ts` (kernel cwd is now the notebook dir — fixed in commit `ab0bf5267`), and a drift-guard Rust test asserts the vendored copies stay byte-identical to the canonical SDK.

**Context for the engineer (read before starting):**
- The kernel now starts with cwd = the notebook's directory (commits `ee32ca2e6` + `ab0bf5267`), so `Deno.cwd()` inside `app.ipynb` is the app root.
- Only `call_tool.ts` and `wire.ts` are vendorable: they import nothing outside themselves (`call_tool.ts` imports `./wire.ts`). `ports.ts` imports `@std/path`, which requires the `deno.json` import map that is NOT available in the kernel context (documented inside `app.ipynb` cell 1) — do not vendor it.
- `export_spur_app` (in `spur_app.rs`) synthesizes a minimal manifest and does NOT read the authored `spur-app.json`; it also doesn't bundle `server/`, `templates/`, or `skill/`. Export/import parity for full apps is a pre-existing gap and **explicitly out of scope** here. Do not touch `export_spur_app`/`import_spur_app`.
- The working tree of `app_gallery/html_video/app.ipynb` is already modified (daemon re-serialization plus a broken `../../ln/src/call_tool.ts` import edit). Task 4 rewrites the import lines; commit the whole notebook file as it stands after your edits.
- Build/test ONLY via `scripts/spur-cargo` (never bare `cargo`). The message `remote VM unavailable (exit 200) — falling back to local` is normal. rustfmt "unstable features" warnings during commits are pre-existing noise.
- The worktree contains unrelated dirty files. Stage files by explicit path only — never `git add -A`/`git add .`.
- Every commit message ends with the trailer: `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

**Tech Stack:** Rust (serde, tokio tests), Deno/TypeScript SDK modules, Jupyter notebook JSON.

---

### Task 1: `sdk` block in the Spur app manifest

**Files:**
- Modify: `crates/spur-notebook/src/spur_app.rs` (struct defs around lines 26–136; `SpurAppManifest::minimal` around line 170)

- [ ] **Step 1: Write the failing round-trip test**

Find the `#[cfg(test)] mod tests` at the bottom of `spur_app.rs` (if the file has none, create one at the bottom: `#[cfg(test)] mod tests { use super::*; … }`). Add:

```rust
#[test]
fn manifest_sdk_block_round_trips_and_defaults_to_none() {
    // Absent field → None (backward compatible with existing manifests).
    let minimal = SpurAppManifest::minimal("App", "app.ipynb");
    assert!(minimal.sdk.is_none());
    let json = serde_json::to_string(&minimal).expect("serialize minimal");
    let back: SpurAppManifest = serde_json::from_str(&json).expect("deserialize minimal");
    assert!(back.sdk.is_none());

    // Declared block round-trips.
    let mut manifest = SpurAppManifest::minimal("App", "app.ipynb");
    manifest.sdk = Some(SpurAppSdk {
        typescript: Some("sdk".to_string()),
    });
    let json = serde_json::to_string(&manifest).expect("serialize with sdk");
    let back: SpurAppManifest = serde_json::from_str(&json).expect("deserialize with sdk");
    assert_eq!(
        back.sdk,
        Some(SpurAppSdk {
            typescript: Some("sdk".to_string())
        })
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `scripts/spur-cargo test -p spur-notebook manifest_sdk_block`
Expected: compile error — `sdk` field and `SpurAppSdk` do not exist yet. That is the RED for an API-introducing change.

- [ ] **Step 3: Add the field and struct**

In `SpurAppManifest`, after the `skill` field (line ~51), add:

```rust
    /// Vendored SDK declarations. When present, the doctor verifies the
    /// referenced directories exist inside the app root. Absent in existing
    /// manifests → `None` (backward compatible).
    #[serde(default)]
    pub sdk: Option<SpurAppSdk>,
```

Below `SpurAppDependencies` (line ~136), add:

```rust
/// Vendored SDK directories, relative to the app root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppSdk {
    /// Directory containing the vendored TypeScript SDK modules
    /// (e.g. `"sdk"` → `<app_root>/sdk/call_tool.ts`).
    #[serde(default)]
    pub typescript: Option<String>,
}
```

In `SpurAppManifest::minimal` (line ~170), add `sdk: None,` to the struct literal.

- [ ] **Step 4: Run the test to verify it passes, plus the crate's manifest tests**

Run: `scripts/spur-cargo test -p spur-notebook manifest_sdk_block` → PASS
Run: `scripts/spur-cargo test -p spur-notebook spur_app` → all existing spur_app tests still PASS (the new field must not break `import_spur_app`/doctor manifest parsing — `#[serde(default)]` guarantees old manifests load).

- [ ] **Step 5: Commit (test + impl together is fine here as one TDD cycle, but repo convention prefers two commits)**

```bash
git add crates/spur-notebook/src/spur_app.rs
git commit -m "feat(spur-notebook): declare vendored sdk block in spur-app manifest"
```

(If you committed the failing test separately first as `test(spur-notebook): …`, even better — follow the repo's test-then-fix cadence.)

---

### Task 2: Doctor check for the declared SDK

**Files:**
- Modify: `crates/spur-notebook/src/mcp/tools/notebook_app_doctor.rs`
- Test: `crates/spur-notebook/tests/notebook_app_doctor.rs` (existing integration test file — read its harness first and follow its fixture pattern for creating a temp app root with a `spur-app.json`)

Required modules for a TypeScript SDK dir: `call_tool.ts` and `wire.ts`.

- [ ] **Step 1: Write the failing integration tests**

In `crates/spur-notebook/tests/notebook_app_doctor.rs`, following the file's existing pattern for invoking the doctor against a temp app root, add two tests:

```rust
// Names/scaffolding must follow the existing harness in this file; the
// behavioral assertions are:

#[tokio::test]
async fn doctor_fails_when_declared_sdk_dir_is_missing() {
    // App root: valid minimal spur-app.json that ALSO contains
    //   "sdk": { "typescript": "sdk" }
    // but NO sdk/ directory on disk.
    // Run the doctor; assert findings contain one with:
    //   check == "sdk:typescript", level == "fail"
    // and overall ok == false.
}

#[tokio::test]
async fn doctor_passes_when_declared_sdk_dir_has_required_modules() {
    // Same app root, but create sdk/call_tool.ts and sdk/wire.ts
    // (any non-empty contents).
    // Assert a finding with check == "sdk:typescript", level == "pass",
    // and no "fail"-level sdk finding.
}
```

Write them as real tests against the existing harness (most tests in that file build a temp dir, write `spur-app.json` + `app.ipynb`, then call the doctor tool and inspect the JSON findings). A manifest missing the `sdk` block entirely must produce NO `sdk:*` finding — assert that in whichever existing happy-path test is cheapest to extend, or add a third small test.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `scripts/spur-cargo test -p spur-notebook --test notebook_app_doctor sdk`
Expected: FAIL — no `sdk:typescript` finding is emitted yet.

- [ ] **Step 3: Commit the failing tests**

```bash
git add crates/spur-notebook/tests/notebook_app_doctor.rs
git commit -m "test(spur-notebook): doctor verifies declared vendored sdk dir"
```

- [ ] **Step 4: Implement the check**

In `notebook_app_doctor.rs`, add a sync helper and call it from `call()` after the capability checks (the function reads the lenient `manifest_value: Value`, same as the capability checks do):

```rust
/// Check 6: declared vendored SDK directories exist with required modules.
fn check_sdk(app_root: &Path, manifest_value: &Value, findings: &mut Vec<Finding>) {
    const REQUIRED_TS_MODULES: &[&str] = &["call_tool.ts", "wire.ts"];

    let Some(ts_dir) = manifest_value["sdk"]["typescript"].as_str() else {
        return; // no sdk block declared — nothing to check
    };

    let dir = app_root.join(ts_dir);
    if !dir.is_dir() {
        findings.push(
            Finding::fail(
                "sdk:typescript",
                format!("declared sdk.typescript dir {ts_dir:?} not found in app root"),
            )
            .with_location(dir.display().to_string()),
        );
        return;
    }

    let missing: Vec<&str> = REQUIRED_TS_MODULES
        .iter()
        .copied()
        .filter(|module| !dir.join(module).is_file())
        .collect();
    if missing.is_empty() {
        findings.push(Finding::pass(
            "sdk:typescript",
            format!("vendored TypeScript SDK present at {ts_dir:?}"),
        ));
    } else {
        findings.push(
            Finding::fail(
                "sdk:typescript",
                format!("vendored TypeScript SDK at {ts_dir:?} is missing {missing:?}"),
            )
            .with_location(dir.display().to_string()),
        );
    }
}
```

Wire it into `call()` in the position after the capabilities checks and before the ports checks (order of `findings` matters only for readability; mirror the doc-comment numbering at the top of the file — bump it to mention the new check).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `scripts/spur-cargo test -p spur-notebook --test notebook_app_doctor`
Expected: all tests PASS (new and pre-existing).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/mcp/tools/notebook_app_doctor.rs
git commit -m "feat(spur-notebook): doctor check for declared vendored sdk"
```

---

### Task 3: Drift guard — vendored SDK must stay byte-identical to the canonical SDK

**Files:**
- Create: `crates/spur-notebook/tests/app_gallery_sdk_drift.rs`

- [ ] **Step 1: Write the test (it will FAIL until Task 4 vendors the files — that ordering is intentional; commit it together with Task 4's vendoring)**

```rust
//! Guards against drift between the canonical TypeScript SDK and copies
//! vendored into app_gallery apps. If this fails, re-copy the listed files
//! from sdk/typescript/src/ into the app's sdk/ directory.

use std::path::{Path, PathBuf};

const VENDORED_MODULES: &[&str] = &["call_tool.ts", "wire.ts"];

fn repo_root() -> PathBuf {
    // crates/spur-notebook → repo root is two levels up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn html_video_vendored_sdk_matches_canonical_sdk() {
    let root = repo_root();
    let canonical = root.join("sdk/typescript/src");
    let vendored = root.join("app_gallery/html_video/sdk");

    for module in VENDORED_MODULES {
        let canonical_bytes = std::fs::read(canonical.join(module))
            .unwrap_or_else(|e| panic!("read canonical {module}: {e}"));
        let vendored_bytes = std::fs::read(vendored.join(module))
            .unwrap_or_else(|e| panic!("read vendored {module}: {e}"));
        assert_eq!(
            canonical_bytes, vendored_bytes,
            "app_gallery/html_video/sdk/{module} has drifted from sdk/typescript/src/{module}; \
             re-copy the canonical file"
        );
    }
}
```

- [ ] **Step 2: Run it to verify it fails for the right reason**

Run: `scripts/spur-cargo test -p spur-notebook --test app_gallery_sdk_drift`
Expected: FAIL with `read vendored call_tool.ts` (file does not exist yet).

(Do not commit yet — Task 4 commits this test together with the vendored files so the tree is never red.)

---

### Task 4: Vendor the SDK into html_video and fix the notebook imports

**Files:**
- Create: `app_gallery/html_video/sdk/call_tool.ts` (byte copy of `sdk/typescript/src/call_tool.ts`)
- Create: `app_gallery/html_video/sdk/wire.ts` (byte copy of `sdk/typescript/src/wire.ts`)
- Modify: `app_gallery/html_video/spur-app.json` (add `sdk` block)
- Modify: `app_gallery/html_video/app.ipynb` (imports in 4 code cells)

- [ ] **Step 1: Copy the SDK files**

```bash
mkdir -p app_gallery/html_video/sdk
cp sdk/typescript/src/call_tool.ts app_gallery/html_video/sdk/call_tool.ts
cp sdk/typescript/src/wire.ts app_gallery/html_video/sdk/wire.ts
```

- [ ] **Step 2: Declare the block in `spur-app.json`**

Add to the top-level JSON object (after `"dependencies"`):

```json
  "sdk": {
    "typescript": "sdk"
  },
```

- [ ] **Step 3: Update the notebook imports**

Edit `app_gallery/html_video/app.ipynb` (it is notebook JSON — edit the `source` arrays; preserve everything else). Four cells reference the SDK:

1. Cell id `html-video-template-search`, the dynamic import. Replace the import statement and its comment block with:

```ts
// TODO(U7): replace with `import { callTool } from "jsr:@spur/app"` once published to JSR.
// The SDK is vendored into the app root (see spur-app.json "sdk"); the kernel
// starts in the notebook's directory, so Deno.cwd() is the app root.
const { callTool } = await import(
  `file://${Deno.cwd()}/sdk/call_tool.ts`
);
```

(The current working-tree source says `file://${Deno.cwd()}/../../ln/src/call_tool.ts` — a broken path; the original was `../../sdk/typescript/src/call_tool.ts`. Either way, replace it.)

2. Cells `html-video-template-preview`, `html-video-render-controls`, and `spur-ad-render` each have a type-only annotation:

```ts
const callTool = (globalThis as any).callTool as typeof import("../../ln/src/call_tool.ts").callTool;
```

Replace the specifier in all three with the vendored path:

```ts
const callTool = (globalThis as any).callTool as typeof import("./sdk/call_tool.ts").callTool;
```

- [ ] **Step 4: Verify the import actually resolves under Deno from the app root (skip only if no `deno` binary is on PATH — note it in your report)**

```bash
cd app_gallery/html_video && deno eval 'const m = await import(`file://${Deno.cwd()}/sdk/call_tool.ts`); if (typeof m.callTool !== "function") throw new Error("callTool missing"); console.log("ok");'
```

Expected output: `ok`

- [ ] **Step 5: Run the drift guard and the doctor against the real app**

Run: `scripts/spur-cargo test -p spur-notebook --test app_gallery_sdk_drift`
Expected: PASS

- [ ] **Step 6: Commit (vendored files + manifest + notebook + drift test together)**

```bash
git add app_gallery/html_video/sdk app_gallery/html_video/spur-app.json app_gallery/html_video/app.ipynb crates/spur-notebook/tests/app_gallery_sdk_drift.rs
git commit -m "feat(html_video): vendor TypeScript SDK into app root"
```

---

### Task 5: Final verification sweep

- [ ] **Step 1: Full crate test run**

Run: `scripts/spur-cargo test -p spur-notebook`
Expected: PASS (all pre-existing + new tests).

- [ ] **Step 2: Clippy**

Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-notebook -- -D warnings` (if the remote VM is unreachable, plain `scripts/spur-cargo clippy -p spur-notebook -- -D warnings` locally is acceptable; report which ran).
Expected: clean.

- [ ] **Step 3: Report**

Report commit SHAs, test output lines, and whether the `deno eval` probe ran.

---

## Out of scope (do not do)

- `export_spur_app`/`import_spur_app` bundling of the sdk dir — the exporter currently synthesizes a minimal manifest and doesn't bundle `server/`/`templates/`/`skill/` either; full bundle parity is a separate workstream.
- Vendoring `ports.ts`/`display.ts`/`capture.ts` — `ports.ts` cannot be file://-imported in the kernel context (needs the import map), and the app doesn't use the others.
- JSR publishing (`TODO(U7)` stays in the notebook comment).
- Any change to the kernel/cwd plumbing (already done in `ee32ca2e6`/`ab0bf5267`).
