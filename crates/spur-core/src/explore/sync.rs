use crate::explore::catalog::{self, Catalog};
use crate::explore::pool::{Manifest, SourceSpec};
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn cache_dir(root: &Path, repo: &str) -> PathBuf {
    root.join(".spur/explore/cache")
        .join(repo.replace(['/', '\\'], "-"))
}

fn ensure_cache_checkout(root: &Path, src: &SourceSpec) -> anyhow::Result<(PathBuf, String)> {
    let dir = cache_dir(root, &src.repo);
    let dir_arg = dir.to_string_lossy().to_string();
    let url = src
        .url
        .clone()
        .unwrap_or_else(|| format!("https://github.com/{}.git", src.repo));

    if dir.exists() {
        run_git(&["-C", &dir_arg, "fetch", "origin"])?;
    } else {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        run_git(&["clone", &url, &dir_arg])?;
    }

    run_git(&["-C", &dir_arg, "checkout", "--detach", &src.pin])?;
    let head = run_git(&["-C", &dir_arg, "rev-parse", "HEAD"])?;
    let resolved = String::from_utf8(head)
        .context("git rev-parse HEAD returned non-utf8 output")?
        .trim()
        .to_string();
    Ok((dir, resolved))
}

pub fn sync(root: &Path, manifest: &Manifest) -> anyhow::Result<Catalog> {
    let mut entries = Vec::new();

    for source in &manifest.sources {
        let (checkout, pinned_commit) = ensure_cache_checkout(root, source)
            .with_context(|| format!("sync source {}", source.repo))?;
        let mut source_entries =
            catalog::scan_source_checkout(&checkout, &source.repo, &pinned_commit)
                .with_context(|| format!("scan source {}", source.repo))?;
        entries.append(&mut source_entries);
    }

    let synced_at_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs();
    let catalog = Catalog {
        synced_at_epoch: Some(synced_at_epoch),
        entries,
    };
    catalog.save(root)?;
    Ok(catalog)
}

fn run_git(args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| anyhow::anyhow!("git spawn: {error}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::pool::{Manifest, SourceSpec};
    use std::path::Path;

    #[test]
    fn sync_clones_pinned_source_and_builds_catalog() {
        let td = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        init_fixture_repo(fixture.path(), "api-design");
        let head = git_stdout(fixture.path(), &["rev-parse", "HEAD"]);
        let manifest = Manifest {
            sources: vec![SourceSpec {
                repo: "acme/repo".to_string(),
                url: Some(fixture.path().display().to_string()),
                pin: head.clone(),
            }],
            items: vec![],
        };

        let catalog = sync(td.path(), &manifest).unwrap();

        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].pinned_commit, head);
        assert_eq!(catalog.entries[0].source, "acme/repo");
        assert!(td
            .path()
            .join(".spur/explore/cache/acme-repo/.git")
            .exists());
        assert!(td.path().join(".spur/explore/index/catalog.json").exists());
        assert_eq!(sync(td.path(), &manifest).unwrap().entries.len(), 1);
    }

    #[test]
    fn sync_failure_names_source_and_leaves_existing_index_untouched() {
        let td = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        init_fixture_repo(fixture.path(), "api-design");
        let good_head = git_stdout(fixture.path(), &["rev-parse", "HEAD"]);
        let existing = crate::explore::catalog::Catalog {
            synced_at_epoch: Some(1),
            entries: vec![],
        };
        existing.save(td.path()).unwrap();
        let before =
            std::fs::read_to_string(td.path().join(".spur/explore/index/catalog.json")).unwrap();
        let manifest = Manifest {
            sources: vec![
                SourceSpec {
                    repo: "acme/repo".to_string(),
                    url: Some(fixture.path().display().to_string()),
                    pin: good_head,
                },
                SourceSpec {
                    repo: "bad/source".to_string(),
                    url: Some(fixture.path().display().to_string()),
                    pin: "missing-ref".to_string(),
                },
            ],
            items: vec![],
        };

        let error = sync(td.path(), &manifest).unwrap_err();

        assert!(format!("{error:#}").contains("bad/source"));
        let after =
            std::fs::read_to_string(td.path().join(".spur/explore/index/catalog.json")).unwrap();
        assert_eq!(after, before);
    }

    fn init_fixture_repo(repo: &Path, skill_name: &str) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "test@spur"]);
        git(repo, &["config", "user.name", "Test User"]);
        write_fixture_skill(repo, skill_name);
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "fixture"]);
    }

    fn write_fixture_skill(repo: &Path, skill_name: &str) {
        let skill_dir = repo.join("skills").join(skill_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {skill_name}\ndescription: Fixture skill\n---\nbody\n"),
        )
        .unwrap();
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
}
