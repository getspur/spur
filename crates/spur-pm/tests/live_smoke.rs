//! T-6 — live smoke test against a real public GitHub repo.
//!
//! Ignored by default. Run manually after a checkout to verify the full
//! end-to-end ingest path against `octocat/Hello-World`:
//!
//! ```bash
//! SPUR_GITHUB_TOKEN=$(gh auth token) \
//!     cargo test -p spur-pm --test live_smoke -- --ignored --nocapture
//! ```
//!
//! Pre-conditions:
//! - `SPUR_GITHUB_TOKEN` is set OR `gh auth status` reports a logged-in user.
//! - Network access to `api.github.com`.
//!
//! Post-conditions:
//! - `fetch_changes_since(None)` returns ≥1 node from `octocat/Hello-World`.
//! - `apply_remote_delta` lands those nodes into a fresh tempdir `.beads/`
//!   with `spur-sync v1` sentinels.
//! - Re-running the same flow on the same tempdir is idempotent (no
//!   duplicate ingests).

use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm::ingest::github::GitHubSync;
use spur_pm::ingest::{apply_remote_delta, IngestOptions};
use spur_pm::sync::ExternalPmSync;

#[tokio::test]
#[ignore = "live smoke; requires SPUR_GITHUB_TOKEN or gh CLI auth + network"]
async fn live_ingest_octocat_hello_world() {
    let sync = GitHubSync::connect("octocat/Hello-World")
        .await
        .expect("token + client");

    let delta = sync
        .fetch_changes_since(None)
        .await
        .expect("fetch_changes_since against octocat/Hello-World");
    assert!(
        !delta.nodes.is_empty(),
        "expected at least one issue/PR from octocat/Hello-World; got {} nodes",
        delta.nodes.len()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let beads_dir = dir.path().join(".beads");
    std::fs::create_dir_all(&beads_dir).unwrap();
    let beads = BeadsCrateAdapter::open(
        &beads_dir,
        AdapterConfig {
            actor: "live-smoke".into(),
            ..Default::default()
        },
    )
    .await
    .expect("open beads in tempdir");

    let opts = IngestOptions::default();
    let report = apply_remote_delta(&beads, &sync as &dyn ExternalPmSync, delta.clone(), &opts)
        .await
        .expect("apply_remote_delta");
    assert!(report.ingested + report.updated + report.unchanged > 0);

    // Idempotency: re-running yields no fresh ingests.
    let report2 = apply_remote_delta(&beads, &sync as &dyn ExternalPmSync, delta, &opts)
        .await
        .expect("apply_remote_delta re-run");
    assert_eq!(
        report2.ingested, 0,
        "second run must not ingest new issues (idempotency violated)"
    );
}
