//! Canonical agent-profile definitions (spec D1): Claude agent markdown
//! stored under `.spur/agents/<name>.md`. Frontmatter `model`/`effort`
//! act as defaults under D8 precedence (request -> profile -> agent default).

use anyhow::{bail, Context, Result};
use std::path::Path;

pub mod render;

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
        let rest = raw
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

    /// Read `.spur/agents/<name>.md` under `repo_root`. `Ok(None)` when the
    /// file does not exist (pass-through selection, spec D4). Parse errors
    /// are hard errors (spec D7).
    pub fn load(repo_root: &Path, name: &str) -> Result<Option<Self>> {
        if name.is_empty() || name.contains('/') || name.contains("..") || name.contains('\\') {
            bail!("invalid agent profile name: {name}");
        }

        let path = repo_root.join(".spur/agents").join(format!("{name}.md"));
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };

        Self::parse(name, &raw).map(Some)
    }
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
}
