//! Canonical agent-profile definitions (spec D1): Claude agent markdown
//! stored under `.spur/agents/<name>.md`. Frontmatter `model`/`effort`
//! act as defaults under D8 precedence (request -> profile -> agent default).
//!
//! When a committed profile is absent, [`AgentProfile::load`] also resolves
//! accepted explore-pool personas (layered local + global store) so mention
//! pickers and delegation can address pool agents without a separate Apply
//! step that writes `.spur/agents/`.

use anyhow::{bail, Context, Result};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub mod render;

fn is_materializable_gate(verdict: &str) -> bool {
    matches!(verdict, "clean" | "overridden" | "replaced-bundled")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub tools: Option<String>,
    pub body: String,
    pub raw: String,
}

impl AgentProfile {
    pub fn parse(expected_name: &str, raw: &str) -> Result<Self> {
        let normalized = if raw.contains("\r\n") {
            Cow::Owned(raw.replace("\r\n", "\n"))
        } else {
            Cow::Borrowed(raw)
        };
        let rest = normalized
            .strip_prefix("---\n")
            .context("agent profile missing YAML frontmatter fence")?;
        let idx = rest
            .find("\n---\n")
            .context("agent profile frontmatter not terminated")?;
        let (frontmatter, body) = (&rest[..idx], &rest[idx + 5..]);

        let mut name = None;
        let mut description = None;
        let mut model = None;
        let mut effort = None;
        let mut tools = None;
        for line in frontmatter.lines() {
            if let Some(value) = line.strip_prefix("name:") {
                name = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("description:") {
                description = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("model:") {
                model = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("effort:") {
                effort = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("tools:") {
                tools = Some(value.trim().to_string());
            }
        }

        let name = name.context("agent profile frontmatter missing `name`")?;
        if name != expected_name {
            bail!("agent profile name `{name}` does not match file name `{expected_name}`");
        }
        let description = description.context("agent profile frontmatter missing `description`")?;
        if description.trim().is_empty() {
            bail!("agent profile frontmatter description must be non-empty");
        }

        Ok(Self {
            name,
            description,
            model,
            effort,
            tools,
            body: body.to_string(),
            raw: raw.to_string(),
        })
    }

    /// Read a profile under `repo_root`.
    ///
    /// Resolution order:
    /// 1. Committed `.spur/agents/<name>.md` (Apply / hand-authored)
    /// 2. Accepted explore-pool agent body (layered local + global store)
    ///
    /// `Ok(None)` when neither source has the name (pass-through selection,
    /// spec D4). Parse errors are hard errors (spec D7).
    pub fn load(repo_root: &Path, name: &str) -> Result<Option<Self>> {
        if name.is_empty() || name.contains('/') || name.contains("..") || name.contains('\\') {
            bail!("invalid agent profile name: {name}");
        }

        let path = repo_root.join(".spur/agents").join(format!("{name}.md"));
        match std::fs::read_to_string(&path) {
            Ok(raw) => return Self::parse(name, &raw).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        }

        Self::load_from_explore_pool(repo_root, name)
    }

    /// Names available for mention pickers / discovery: committed
    /// `.spur/agents/*.md` ∪ accepted explore-pool agents (deduped, sorted).
    pub fn candidate_names(repo_root: &Path) -> Vec<String> {
        let mut names = BTreeSet::new();

        let dir = repo_root.join(".spur/agents");
        if let Ok(read_dir) = std::fs::read_dir(&dir) {
            for entry in read_dir.filter_map(Result::ok) {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !stem.is_empty()
                        && !stem.contains('/')
                        && !stem.contains("..")
                        && !stem.contains('\\')
                    {
                        names.insert(stem.to_string());
                    }
                }
            }
        }

        match crate::explore::pool::Manifest::load_layered(repo_root) {
            Ok(manifest) => {
                for item in manifest.items {
                    if item.kind == crate::explore::catalog::ItemKind::Agent
                        && is_materializable_gate(&item.gate.verdict)
                    {
                        names.insert(item.name);
                    }
                }
            }
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "Skipping explore pool while listing agent profile candidates"
                );
            }
        }

        names.into_iter().collect()
    }

    fn load_from_explore_pool(repo_root: &Path, name: &str) -> Result<Option<Self>> {
        let manifest = match crate::explore::pool::Manifest::load_layered(repo_root) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::debug!(
                    profile = %name,
                    error = %error,
                    "Failed to load explore manifest for agent profile fallback"
                );
                return Ok(None);
            }
        };

        let Some(item) = manifest.items.iter().find(|item| {
            item.kind == crate::explore::catalog::ItemKind::Agent && item.name == name
        }) else {
            return Ok(None);
        };

        if !is_materializable_gate(&item.gate.verdict) {
            return Ok(None);
        }

        let path = pool_agent_body_path(repo_root, item);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };

        Self::parse(name, &raw).map(Some)
    }
}

fn pool_agent_body_path(repo_root: &Path, item: &crate::explore::pool::ManifestItem) -> PathBuf {
    let dir = crate::explore::store::layered_pool_dir(
        repo_root,
        &item.source,
        &item.name,
        &item.pinned_commit,
    );
    let file_name = Path::new(&item.rel_path)
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from(format!("{}.md", item.name)));
    dir.join(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "---\nname: code-reviewer\ndescription: Reviews diffs for correctness\nmodel: opus\neffort: high\ntools: Read, Grep\n---\nYou are a rigorous code reviewer.\n";

    #[test]
    fn parses_all_frontmatter_fields_and_body() {
        let p = AgentProfile::parse("code-reviewer", RAW).unwrap();
        assert_eq!(p.name, "code-reviewer");
        assert_eq!(p.description, "Reviews diffs for correctness");
        assert_eq!(p.model.as_deref(), Some("opus"));
        assert_eq!(p.effort.as_deref(), Some("high"));
        assert_eq!(p.tools.as_deref(), Some("Read, Grep"));
        assert_eq!(p.body.trim(), "You are a rigorous code reviewer.");
        assert_eq!(p.raw, RAW);
    }

    #[test]
    fn minimal_profile_needs_only_name_description_body() {
        let raw = "---\nname: minimal\ndescription: d\n---\nbody\n";
        let p = AgentProfile::parse("minimal", raw).unwrap();
        assert!(p.model.is_none() && p.effort.is_none() && p.tools.is_none());
    }

    #[test]
    fn parses_crlf_line_endings() {
        let raw = "---\r\nname: windows\r\ndescription: d\r\n---\r\nbody\r\n";
        let p = AgentProfile::parse("windows", raw).unwrap();
        assert_eq!(p.name, "windows");
        assert_eq!(p.description, "d");
        assert_eq!(p.body, "body\n");
        assert_eq!(p.raw, raw);
    }

    #[test]
    fn frontmatter_name_mismatch_is_error() {
        let raw = "---\nname: other\ndescription: d\n---\nbody\n";
        assert!(AgentProfile::parse("minimal", raw).is_err());
    }

    #[test]
    fn missing_frontmatter_or_description_is_error() {
        assert!(AgentProfile::parse("x", "no frontmatter").is_err());
        assert!(AgentProfile::parse("x", "---\nname: x\n---\nbody\n").is_err());
    }

    #[test]
    fn empty_description_is_error() {
        assert!(AgentProfile::parse("x", "---\nname: x\n---\nbody\n").is_err());
        let error = AgentProfile::parse("x", "---\nname: x\ndescription:\n---\nbody\n")
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "agent profile frontmatter description must be non-empty"
        );
    }

    #[test]
    fn load_reads_spur_agents_dir_and_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".spur/agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("code-reviewer.md"), RAW).unwrap();
        assert!(AgentProfile::load(tmp.path(), "code-reviewer")
            .unwrap()
            .is_some());
        assert!(AgentProfile::load(tmp.path(), "absent").unwrap().is_none());
    }

    #[test]
    fn load_rejects_path_traversal_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(AgentProfile::load(tmp.path(), "../evil").is_err());
        assert!(AgentProfile::load(tmp.path(), "a/b").is_err());
    }

    fn write_pool_agent(
        root: &Path,
        name: &str,
        source: &str,
        pin: &str,
        body: &str,
        gate_verdict: &str,
    ) {
        use crate::explore::catalog::ItemKind;
        use crate::explore::pool::{GateRecord, Manifest, ManifestItem};

        let pool_dir = crate::explore::store::local_pool_dir(root, source, name, pin);
        std::fs::create_dir_all(&pool_dir).unwrap();
        std::fs::write(pool_dir.join(format!("{name}.md")), body).unwrap();

        Manifest {
            sources: vec![],
            items: vec![ManifestItem {
                name: name.to_string(),
                kind: ItemKind::Agent,
                source: source.to_string(),
                rel_path: format!("agents/{name}.md"),
                pinned_commit: pin.to_string(),
                content_sha256: "0".repeat(64),
                license: None,
                gate: GateRecord {
                    verdict: gate_verdict.to_string(),
                    justification: None,
                    decided_at_epoch: None,
                },
            }],
        }
        .save(root)
        .unwrap();
    }

    #[test]
    fn load_falls_back_to_explore_pool_when_spur_agents_absent() {
        let _global = crate::explore::store::force_global_root_for_tests(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let pin = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        write_pool_agent(
            tmp.path(),
            "rust-engineer",
            "acme/agents",
            pin,
            "---\nname: rust-engineer\ndescription: Systems Rust\n---\npool body\n",
            "clean",
        );

        let profile = AgentProfile::load(tmp.path(), "rust-engineer")
            .unwrap()
            .expect("pool agent should load without .spur/agents");
        assert_eq!(profile.name, "rust-engineer");
        assert_eq!(profile.description, "Systems Rust");
        assert_eq!(profile.body.trim(), "pool body");
    }

    #[test]
    fn load_prefers_committed_spur_agents_over_explore_pool() {
        let _global = crate::explore::store::force_global_root_for_tests(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let pin = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        write_pool_agent(
            tmp.path(),
            "code-reviewer",
            "acme/agents",
            pin,
            "---\nname: code-reviewer\ndescription: Pool copy\n---\npool\n",
            "clean",
        );
        let dir = tmp.path().join(".spur/agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("code-reviewer.md"),
            "---\nname: code-reviewer\ndescription: Committed copy\n---\ncommitted\n",
        )
        .unwrap();

        let profile = AgentProfile::load(tmp.path(), "code-reviewer")
            .unwrap()
            .expect("committed profile present");
        assert_eq!(profile.description, "Committed copy");
        assert_eq!(profile.body.trim(), "committed");
    }

    #[test]
    fn load_skips_pool_agent_with_non_materializable_gate() {
        let _global = crate::explore::store::force_global_root_for_tests(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let pin = "cccccccccccccccccccccccccccccccccccccccc";
        write_pool_agent(
            tmp.path(),
            "blocked-agent",
            "acme/agents",
            pin,
            "---\nname: blocked-agent\ndescription: Blocked\n---\nbody\n",
            "blocked",
        );

        assert!(AgentProfile::load(tmp.path(), "blocked-agent")
            .unwrap()
            .is_none());
    }

    #[test]
    fn candidate_names_includes_explore_pool_agents_without_spur_agents_dir() {
        let _global = crate::explore::store::force_global_root_for_tests(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let pin = "dddddddddddddddddddddddddddddddddddddddd";
        write_pool_agent(
            tmp.path(),
            "rust-pro",
            "acme/agents",
            pin,
            "---\nname: rust-pro\ndescription: Rust pro\n---\nbody\n",
            "clean",
        );

        let names = AgentProfile::candidate_names(tmp.path());
        assert!(
            names.iter().any(|n| n == "rust-pro"),
            "expected rust-pro in {names:?}"
        );
    }

    #[test]
    fn candidate_names_merges_committed_and_pool_without_duplicates() {
        let _global = crate::explore::store::force_global_root_for_tests(None);
        let tmp = tempfile::TempDir::new().unwrap();
        let pin = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        write_pool_agent(
            tmp.path(),
            "shared",
            "acme/agents",
            pin,
            "---\nname: shared\ndescription: Pool shared\n---\npool\n",
            "clean",
        );
        let dir = tmp.path().join(".spur/agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("shared.md"),
            "---\nname: shared\ndescription: Local shared\n---\nlocal\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("local-only.md"),
            "---\nname: local-only\ndescription: Local only\n---\nlocal\n",
        )
        .unwrap();

        let names = AgentProfile::candidate_names(tmp.path());
        assert_eq!(
            names.iter().filter(|n| *n == "shared").count(),
            1,
            "shared must appear once: {names:?}"
        );
        assert!(names.iter().any(|n| n == "local-only"));
        assert!(names.iter().any(|n| n == "shared"));
    }
}
