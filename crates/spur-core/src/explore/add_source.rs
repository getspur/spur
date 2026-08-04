//! Add a git URL as an explore catalog source and sync.

use crate::explore::pool::{Manifest, SourceSpec};
use crate::explore::sync;
use std::path::Path;

/// Extract `owner/repo` from a git URL.
///
/// Handles HTTPS (`https://github.com/owner/repo[.git]`) and SSH
/// (`git@github.com:owner/repo[.git]`) formats. Strips trailing `.git`
/// for the repo name but keeps the URL as-is for `SourceSpec.url`.
pub fn parse_git_url_repo(url: &str) -> Result<String, String> {
    let url = url.strip_suffix(".git").unwrap_or(url);
    let path = if url.contains("://") {
        // HTTPS (and similar): skip the host segment after the scheme.
        url.split("://")
            .nth(1)
            .unwrap_or(url)
            .split('/')
            .skip(1)
            .collect::<Vec<_>>()
            .join("/")
    } else {
        // SSH: git@host:owner/repo
        url.split(':').last().unwrap_or(url).to_string()
    };
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 2 {
        return Err("URL must include owner/repo path".to_string());
    }
    Ok(format!(
        "{}/{}",
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    ))
}

/// Append a source to the local explore manifest (if not already present) and
/// sync the layered manifest into `store_root`.
///
/// Dedup is by `repo` name: when the source already exists the write is
/// skipped but sync still runs.
pub fn add_source_and_sync(
    repo_root: &Path,
    url: &str,
    pin: &str,
    store_root: &Path,
) -> anyhow::Result<()> {
    let repo = parse_git_url_repo(url).map_err(anyhow::Error::msg)?;
    let mut manifest = Manifest::load(repo_root)?;
    if !manifest.sources.iter().any(|s| s.repo == repo) {
        manifest.sources.push(SourceSpec {
            repo,
            url: Some(url.to_string()),
            pin: pin.to_string(),
        });
        manifest.save(repo_root)?;
    }
    let layered = Manifest::load_layered(repo_root)?;
    sync::sync_to_store(store_root, &layered)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_git_url_repo;

    #[test]
    fn test_parse_git_url_repo() {
        assert_eq!(
            parse_git_url_repo("https://github.com/owner/repo").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            parse_git_url_repo("https://github.com/owner/repo.git").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            parse_git_url_repo("https://gitlab.com/org/project").unwrap(),
            "org/project"
        );
        assert_eq!(
            parse_git_url_repo("git@github.com:owner/repo.git").unwrap(),
            "owner/repo"
        );
        assert!(parse_git_url_repo("not-a-url").is_err());
        assert!(parse_git_url_repo("https://github.com/").is_err());
    }
}
