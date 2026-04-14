#!/usr/bin/env bash
# Verification script for
#   docs/superpowers/plans/2026-04-14-brain-worker-phase1-refinement.md
#
# Asserts every load-bearing claim the plan makes about current code state:
# file paths, line-number references, struct shapes, construction-site
# counts, dependency availability, toolchain features, and build/test
# baseline. Exits 0 on full PASS, 1 on any FAIL.
#
# Run from repo root:
#     bash scripts/verify-brain-worker-phase1-grounding.sh
#
# Checks are grouped by task number to make stale references easy to
# triage. Warnings do not fail the script but signal plan drift.

set -u

PASS=0
FAIL=0
WARN=0
FAILED_CHECKS=()

ok()   { echo "  ✓ $1"; PASS=$((PASS+1)); }
bad()  { echo "  ✗ $1";
         [ -n "${2:-}" ] && echo "    → $2"
         FAIL=$((FAIL+1)); FAILED_CHECKS+=("$1"); }
warn() { echo "  ⚠ $1";
         [ -n "${2:-}" ] && echo "    → $2"
         WARN=$((WARN+1)); }

section() { echo; echo "══ $1 ══"; }

# ── Helpers ───────────────────────────────────────────────────────────
# assert_file_has_line_near <file> <line_no> <regex>
#   Passes if the regex matches within ±3 lines of the given line.
assert_near() {
    local file=$1 line=$2 pattern=$3 label=$4
    local start=$((line-3)); [ $start -lt 1 ] && start=1
    local end=$((line+3))
    if awk "NR>=$start && NR<=$end" "$file" | grep -qE "$pattern"; then
        ok "$label (≈ L$line)"
    else
        local found=$(grep -nE "$pattern" "$file" | head -3 | tr '\n' ',' | sed 's/,$//')
        bad "$label (expected ≈ L$line in $file)" "pattern not found near line; actual hits: ${found:-none}"
    fi
}

assert_count() {
    local file=$1 pattern=$2 expected=$3 label=$4
    local got=$(grep -cE "$pattern" "$file" 2>/dev/null || echo 0)
    if [ "$got" = "$expected" ]; then
        ok "$label ($got matches as expected)"
    else
        bad "$label" "expected $expected matches of /$pattern/ in $file, got $got"
    fi
}

assert_grep() {
    local file=$1 pattern=$2 label=$3
    if grep -qE "$pattern" "$file" 2>/dev/null; then
        ok "$label"
    else
        bad "$label" "pattern /$pattern/ not found in $file"
    fi
}

assert_not_grep() {
    local file=$1 pattern=$2 label=$3
    if ! grep -qE "$pattern" "$file" 2>/dev/null; then
        ok "$label"
    else
        local hits=$(grep -nE "$pattern" "$file" | head -3 | tr '\n' ',')
        bad "$label" "pattern should NOT exist; found: $hits"
    fi
}

assert_file() {
    local file=$1 label=$2
    if [ -f "$file" ]; then ok "$label"; else bad "$label" "file missing: $file"; fi
}

# ──────────────────────────────────────────────────────────────────────
# 0 · Baseline (toolchain + repo state)
# ──────────────────────────────────────────────────────────────────────
section "0 · Baseline"

if command -v cargo >/dev/null; then
    RUST_VER=$(rustc --version | awk '{print $2}')
    ok "rustc present: $RUST_VER"
    # floor_char_boundary stable since 1.80
    major=$(echo "$RUST_VER" | cut -d. -f1)
    minor=$(echo "$RUST_VER" | cut -d. -f2)
    if [ "$major" -gt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -ge 80 ]; }; then
        ok "rustc ≥ 1.80 (floor_char_boundary / ceil_char_boundary stable)"
    else
        bad "rustc < 1.80" "plan uses floor_char_boundary/ceil_char_boundary which require 1.80+"
    fi
else
    bad "cargo missing" "cannot run any build/test check"
fi

if command -v git >/dev/null; then
    ok "git present"
else
    bad "git missing" "build_diff_summary helper calls git"
fi

if command -v rg >/dev/null; then ok "ripgrep present"; else warn "ripgrep not found" "plan's manual-smoke step uses rg; grep fallback ok"; fi

assert_grep "Cargo.toml" '^rust-version = "1\.' "workspace declares rust-version"

# ──────────────────────────────────────────────────────────────────────
# 1 · Files & deps
# ──────────────────────────────────────────────────────────────────────
section "1 · Files & deps"

ORCH=crates/spur-core/src/orchestrator.rs
SERVER=crates/spur-mcp/src/server.rs
TOOLS=crates/spur-mcp/src/tools.rs
DELEG=crates/spur-acp/src/domain/delegation.rs
EVENTS=crates/spur-acp/src/domain/events.rs
LIB=crates/spur-core/src/lib.rs

assert_file "$ORCH"   "orchestrator.rs present"
assert_file "$SERVER" "spur-mcp/server.rs present"
assert_file "$TOOLS"  "spur-mcp/tools.rs present"
assert_file "$DELEG"  "spur-acp/delegation.rs present"
assert_file "$EVENTS" "spur-acp/events.rs present"
assert_file "$LIB"    "spur-core/lib.rs present"

# tempfile is a dev-dep of spur-core (Task 2 tests)
assert_grep "crates/spur-core/Cargo.toml" '^tempfile *= *"3"' "spur-core: tempfile = \"3\" dev-dep"

# anyhow is a dep of spur-core (Task 2 helper returns anyhow::Result)
assert_grep "crates/spur-core/Cargo.toml" "^anyhow" "spur-core: anyhow dep present"

# tokio with process feature (Task 2 uses tokio::process::Command)
if grep -qE 'tokio.*features.*"process"' crates/spur-core/Cargo.toml \
    || grep -qE 'tokio.*features.*full' crates/spur-core/Cargo.toml \
    || grep -qE '\[dependencies\.tokio\]' crates/spur-core/Cargo.toml; then
    ok "spur-core: tokio features likely include 'process'"
else
    warn "spur-core: tokio 'process' feature unclear" "build_diff_summary may need feature enabled"
fi

# ──────────────────────────────────────────────────────────────────────
# 2 · Existing shape — Task 1 & 2 helpers must not already exist
# ──────────────────────────────────────────────────────────────────────
section "2 · Helpers must not pre-exist"

assert_not_grep "$ORCH" 'fn truncate_summary\b'     "no existing fn truncate_summary"
assert_not_grep "$ORCH" 'fn truncate_summary_env_default' "no existing truncate_summary_env_default"
assert_not_grep "$ORCH" 'async fn build_diff_summary' "no existing build_diff_summary"
assert_not_grep "$ORCH" 'struct RetryAttempt\b'     "no existing struct RetryAttempt"
assert_not_grep "$ORCH" 'fn render_retry_context'   "no existing render_retry_context"
assert_not_grep "$ORCH" 'fn apply_bloat_cap'        "no existing apply_bloat_cap"

# ──────────────────────────────────────────────────────────────────────
# 3 · spur-mcp threading — Task 3
# ──────────────────────────────────────────────────────────────────────
section "3 · spur-mcp pre-state (Task 3)"

# DelegationRequest currently has 5 fields, no brain_session_id
assert_grep "$TOOLS"     'pub struct DelegationRequest'   "DelegationRequest struct exists"
assert_not_grep "$TOOLS" 'brain_session_id'               "DelegationRequest has no brain_session_id yet"

# McpCallbackServer currently has 3 fields, no brain_session_id
assert_grep "$SERVER"     'pub struct McpCallbackServer' "McpCallbackServer struct exists"
assert_not_grep "$SERVER" 'brain_session_id'             "McpCallbackServer has no brain_session_id yet"

# 7 DelegationRequest construction sites expected (not 8 — list_available_workers constructs none)
assert_count "$SERVER" 'DelegationRequest \{' 7 "7 DelegationRequest construction sites in server.rs"

# SessionId must be Clone (the plan clones it at each construction site)
assert_grep "crates/spur-acp/src/types.rs" 'pub struct SessionId' "SessionId defined in spur-acp/types.rs"
if grep -RE '#\[derive\(.*Clone.*\)\].*\n\s*pub struct SessionId' crates/spur-acp/src/types.rs -P >/dev/null 2>&1; then
    ok "SessionId derives Clone"
else
    # Fallback: loose single-line check
    if grep -B2 'pub struct SessionId' crates/spur-acp/src/types.rs | grep -qE 'Clone'; then
        ok "SessionId derives Clone (via nearby derive)"
    else
        bad "SessionId Clone" "struct SessionId may not derive Clone; plan relies on .clone()"
    fi
fi

# ──────────────────────────────────────────────────────────────────────
# 4 · Orchestrator pre-state — Tasks 4, 5, 6, 8
# ──────────────────────────────────────────────────────────────────────
section "4 · Orchestrator pre-state"

# Single destructure site for DelegationRequest at ~L1531
assert_near "$ORCH" 1531 'DelegationRequest \{' "DelegationRequest destructure near L1531"

# execute_delegation at ~L1629
assert_near "$ORCH" 1629 'async fn execute_delegation' "fn execute_delegation near L1629"

# run_one_worker_attempt at ~L2357
assert_near "$ORCH" 2357 'async fn run_one_worker_attempt' "fn run_one_worker_attempt near L2357"

# DelegationRequested emission at ~L2373 (inside run_one_worker_attempt)
assert_near "$ORCH" 2373 'DelegationRequested' "DelegationRequested emission near L2373"

# DelegationDispatched emission at ~L2415
assert_near "$ORCH" 2415 'DelegationDispatched' "DelegationDispatched emission near L2415"

# Both emissions currently use worker_session as 'from'
assert_count "$ORCH" 'from: worker_session\.clone\(\)' 2 "2 emissions currently use from: worker_session.clone()"

# DelegationRequested/Dispatched variants defined exactly once each (not duplicated)
assert_count "$ORCH" 'SpurEventBody::DelegationRequested \{' 1 "exactly 1 DelegationRequested emission site"
assert_count "$ORCH" 'SpurEventBody::DelegationDispatched \{' 1 "exactly 1 DelegationDispatched emission site"

# ReviewPayload at ~L1812-1817 passes diff_summary: None
assert_near "$ORCH" 1814 'diff_summary: None' "ReviewPayload at L~1814 currently passes diff_summary: None"

# WorkerAttemptOutcome at ~L2251 currently lacks diff_summary field
assert_near "$ORCH" 2251 'struct WorkerAttemptOutcome' "WorkerAttemptOutcome struct near L2251"
# Grep the struct body (lines 2251-2262): no diff_summary field
if awk 'NR>=2249 && NR<=2265' "$ORCH" | grep -q 'diff_summary'; then
    bad "WorkerAttemptOutcome pre-state" "diff_summary already present in WorkerAttemptOutcome (unexpected)"
else
    ok "WorkerAttemptOutcome has no diff_summary yet"
fi

# finalize fn at ~L2201
assert_near "$ORCH" 2201 'fn finalize' "fn finalize near L2201"

# Task 6 site: byte-slice summary truncation at ~L2491
assert_near "$ORCH" 2491 'output_text\[\.\.500\]' "byte-slice &output_text[..500] at L~2491 (UTF-8 bug site)"

# Task 6 site: generic error string at ~L2503
assert_near "$ORCH" 2503 'Worker reported errors' "literal \"Worker reported errors\" at L~2503"

# Task 8 site: non-accumulating augmentation at ~L2019
assert_near "$ORCH" 2019 '## Additional constraints' "non-accumulating augmented-task literal at L~2019"

# Task 8 site: Retry match arm at ~L1951
assert_near "$ORCH" 1951 'ReviewDecision::Retry \{ new_constraints \}' "ReviewDecision::Retry match arm near L1951"

# ──────────────────────────────────────────────────────────────────────
# 5 · spur-acp pre-state — Tasks 5, 4
# ──────────────────────────────────────────────────────────────────────
section "5 · spur-acp pre-state"

# DelegationResult shape at ~L53-59
assert_near "$DELEG" 53 'pub struct DelegationResult' "DelegationResult struct near L53"
assert_not_grep "$DELEG" 'diff_summary' "DelegationResult has no diff_summary yet"

# ReviewPayload has diff_summary: Option<DiffSummary> (already)
assert_grep "$EVENTS" 'diff_summary: Option<DiffSummary>' "ReviewPayload already has diff_summary field"

# DiffSummary shape: 4 exact fields (indented `pub <name>:` lines only)
DS_FIELDS=$(awk '/^pub struct DiffSummary/,/^\}/' "$EVENTS" | grep -cE '^\s+pub [a-z_]+:')
if [ "$DS_FIELDS" = "4" ]; then
    ok "DiffSummary has 4 pub fields"
else
    bad "DiffSummary field count" "expected 4 pub fields, got $DS_FIELDS"
fi
assert_grep "$EVENTS" 'files_changed: usize'  "DiffSummary.files_changed: usize"
assert_grep "$EVENTS" 'insertions: usize'     "DiffSummary.insertions: usize"
assert_grep "$EVENTS" 'deletions: usize'      "DiffSummary.deletions: usize"
assert_grep "$EVENTS" 'files: Vec<PathBuf>'   "DiffSummary.files: Vec<PathBuf>"

# DelegationRequested/Dispatched doc comments carry the "pre-existing limitation" caveat
assert_grep "$EVENTS" 'pre-existing limitation' "events.rs still has the 'pre-existing limitation' caveats (Task 4 will remove)"

# DelegationStatus is #[non_exhaustive]
assert_grep "$DELEG" '#\[non_exhaustive\]' "DelegationStatus is #[non_exhaustive]"

# ──────────────────────────────────────────────────────────────────────
# 6 · lib.rs re-exports — Task 8
# ──────────────────────────────────────────────────────────────────────
section "6 · lib.rs re-exports"

# Must re-export DiffSummary (the plan uses spur_acp::DiffSummary in test_support)
assert_grep "crates/spur-acp/src/lib.rs" 'DiffSummary' "spur-acp re-exports DiffSummary"

# spur-core lib.rs: orchestrator module is pub so test_support is reachable
assert_grep "$LIB" 'pub use orchestrator' "spur-core/lib.rs re-exports from orchestrator"

# ──────────────────────────────────────────────────────────────────────
# 7 · Build & test baseline
# ──────────────────────────────────────────────────────────────────────
section "7 · Build & test baseline"

echo "  … running cargo build --workspace (may take a minute)"
if cargo build --workspace 2>/tmp/spur-build.err >/dev/null; then
    ok "cargo build --workspace is green"
else
    bad "baseline build" "cargo build --workspace is RED — see /tmp/spur-build.err"
fi

echo "  … running cargo test --workspace --no-run (compile tests only)"
if cargo test --workspace --no-run 2>/tmp/spur-test-compile.err >/dev/null; then
    ok "tests compile clean"
else
    bad "test compile" "cargo test --no-run is RED — see /tmp/spur-test-compile.err"
fi

# Full test run is expensive; gate behind env var
if [ "${SPUR_VERIFY_FULL_TESTS:-0}" = "1" ]; then
    echo "  … running cargo test --workspace (SPUR_VERIFY_FULL_TESTS=1)"
    if cargo test --workspace --quiet 2>/tmp/spur-test.err >/dev/null; then
        ok "full test suite green"
    else
        bad "baseline tests" "cargo test --workspace has failures — see /tmp/spur-test.err"
    fi
else
    warn "full test run skipped" "set SPUR_VERIFY_FULL_TESTS=1 to run all tests"
fi

# ──────────────────────────────────────────────────────────────────────
# 8 · char_boundary API availability (sanity inline build)
# ──────────────────────────────────────────────────────────────────────
section "8 · str::floor_char_boundary / ceil_char_boundary"

CHECK_DIR=$(mktemp -d)
cat > "$CHECK_DIR/check.rs" <<'EOF'
fn main() {
    let s = "—abc";
    let _ = s.floor_char_boundary(2);
    let _ = s.ceil_char_boundary(2);
    println!("ok");
}
EOF
if rustc --edition=2021 "$CHECK_DIR/check.rs" -o "$CHECK_DIR/check" 2>/tmp/spur-rustc.err >/dev/null; then
    if "$CHECK_DIR/check" | grep -q "ok"; then
        ok "floor/ceil_char_boundary compile + run clean"
    else
        bad "char_boundary runtime" "rustc succeeded but runtime failed"
    fi
else
    bad "floor/ceil_char_boundary unavailable" "rustc failed; see /tmp/spur-rustc.err"
fi
rm -rf "$CHECK_DIR"

# ──────────────────────────────────────────────────────────────────────
# Summary
# ──────────────────────────────────────────────────────────────────────
echo
echo "══════════════════════════════════════════════════════════════════"
echo "  Pass: $PASS   Fail: $FAIL   Warn: $WARN"
echo "══════════════════════════════════════════════════════════════════"

if [ "$FAIL" -gt 0 ]; then
    echo
    echo "Failed checks — update the plan before implementing:"
    for c in "${FAILED_CHECKS[@]}"; do echo "  • $c"; done
    exit 1
fi
exit 0
