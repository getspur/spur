//! End-to-end installer tests using tempdir roots.

use spur_core::skills::installer::run;
use spur_core::skills::SkillSource;
use std::path::Path;

fn count_files_under(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    walkdir_shim(dir).filter(|p| p.is_file()).count()
}

// Minimal recursive walker to avoid a new dev-dep.
fn walkdir_shim(dir: &Path) -> Box<dyn Iterator<Item = std::path::PathBuf>> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir_shim(&p));
            } else {
                out.push(p);
            }
        }
    }
    Box::new(out.into_iter())
}

#[test]
fn fresh_install_creates_all_expected_files() {
    let tmp = tempfile::tempdir().unwrap();

    // Snapshot skill IDs BEFORE run(): running the installer writes
    // .spur/skills/<id>/SKILL.md for every bundled skill, after which
    // list_active_skills would reclassify those same skills as Override.
    let bundled_ids: Vec<String> = spur_core::skills::list_active_skills(tmp.path())
        .unwrap()
        .iter()
        .filter(|s| matches!(s.source, SkillSource::Bundled))
        .map(|s| s.id.clone())
        .collect();
    let bundled_count = bundled_ids.len();

    let summary = run(tmp.path()).unwrap();

    // Every bundled skill × 7 adapters should be written, + Kiro pointer.
    let expected = bundled_count * 7 + 1; // +1 for Kiro pointer
    assert_eq!(
        summary.written.len(),
        expected,
        "expected {expected} writes, got {}",
        summary.written.len(),
    );

    // Every bundled skill should have a file under `.spur/skills/<id>/`.
    for id in &bundled_ids {
        let p = tmp.path().join(".spur/skills").join(id).join("SKILL.md");
        assert!(p.exists(), "missing {}", p.display());
    }

    // Every adapter root should exist.
    for d in [
        ".spur/skills",
        ".claude/skills",
        ".codex/skills",
        ".gemini/skills",
        ".kiro/skills",
        ".kiro/steering",
        ".opencode/skills",
        ".cursor/rules",
    ] {
        let root = tmp.path().join(d);
        assert!(root.is_dir(), "expected dir {}", root.display());
        assert!(count_files_under(&root) > 0, "expected files under {}", root.display());
    }
}

#[test]
fn rerun_is_idempotent_no_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let _first = run(tmp.path()).unwrap();
    let second = run(tmp.path()).unwrap();
    assert!(
        second.written.is_empty(),
        "expected no writes on re-run, got {}: {:?}",
        second.written.len(),
        second.written,
    );
    assert!(
        second.skipped.is_empty(),
        "expected no skips on re-run, got: {:?}",
        second.skipped,
    );
    assert!(!second.unchanged.is_empty(), "expected some NoOps");
}
