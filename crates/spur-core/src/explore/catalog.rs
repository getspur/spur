use anyhow::Context;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Skill,
    Agent,
}

#[expect(
    clippy::derive_partial_eq_without_eq,
    reason = "explore phase plan locks this public derive set"
)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CatalogEntry {
    pub kind: ItemKind,
    pub name: String,
    pub source: String,
    pub rel_path: String,
    pub pinned_commit: String,
    pub description: String,
    pub license: Option<String>,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Catalog {
    pub synced_at_epoch: Option<u64>,
    pub entries: Vec<CatalogEntry>,
}

impl Catalog {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = catalog_path(root);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let path = catalog_path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        let raw = serde_json::to_string_pretty(self).context("serialize explore catalog")?;
        std::fs::write(&path, raw).with_context(|| format!("write {}", path.display()))
    }
}

fn catalog_path(root: &Path) -> PathBuf {
    root.join(".spur/explore/index/catalog.json")
}

pub fn scan_source_checkout(
    checkout: &Path,
    source: &str,
    pinned_commit: &str,
) -> anyhow::Result<Vec<CatalogEntry>> {
    let mut entries = Vec::new();
    scan_dir(checkout, checkout, source, pinned_commit, 0, &mut entries)?;
    entries.sort_by(|a, b| {
        a.kind
            .cmp_key()
            .cmp(&b.kind.cmp_key())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

fn scan_dir(
    root: &Path,
    dir: &Path,
    source: &str,
    pinned_commit: &str,
    depth: usize,
    entries: &mut Vec<CatalogEntry>,
) -> anyhow::Result<()> {
    if depth > 6 {
        return Ok(());
    }

    if dir.join("SKILL.md").is_file() {
        if let Some(entry) = skill_entry(root, dir, source, pinned_commit)? {
            entries.push(entry);
        }
        return Ok(());
    }

    let mut children = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read dir entry {}", dir.display()))?;
        children.push(entry);
    }
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        let path = entry.path();
        let file_name = entry.file_name();
        if file_name == ".git" || file_name == "node_modules" {
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspect {}", path.display()))?;
        if file_type.is_dir() {
            scan_dir(root, &path, source, pinned_commit, depth + 1, entries)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            if let Some(entry) = agent_entry(root, &path, source, pinned_commit)? {
                entries.push(entry);
            }
        }
    }

    Ok(())
}

fn skill_entry(
    root: &Path,
    dir: &Path,
    source: &str,
    pinned_commit: &str,
) -> anyhow::Result<Option<CatalogEntry>> {
    let skill_path = dir.join("SKILL.md");
    let raw = std::fs::read_to_string(&skill_path)
        .with_context(|| format!("read {}", skill_path.display()))?;
    let parsed = crate::skills::frontmatter::parse_source(&raw);
    let description = parsed
        .description
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    if description.is_empty() {
        return Ok(None);
    }

    let fallback_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let name = parsed.name.unwrap_or(fallback_name).trim();
    let name = if name.is_empty() { fallback_name } else { name };

    Ok(Some(CatalogEntry {
        kind: ItemKind::Skill,
        name: name.to_string(),
        source: source.to_string(),
        rel_path: rel_path(root, dir)?,
        pinned_commit: pinned_commit.to_string(),
        description,
        license: frontmatter_value(&raw, "license"),
        content_sha256: crate::explore::content_hash(dir)?,
    }))
}

fn agent_entry(
    root: &Path,
    path: &Path,
    source: &str,
    pinned_commit: &str,
) -> anyhow::Result<Option<CatalogEntry>> {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let Ok(profile) = crate::agent_profiles::AgentProfile::parse(stem, &raw) else {
        return Ok(None);
    };

    Ok(Some(CatalogEntry {
        kind: ItemKind::Agent,
        name: profile.name,
        source: source.to_string(),
        rel_path: rel_path(root, path)?,
        pinned_commit: pinned_commit.to_string(),
        description: profile.description,
        license: None,
        content_sha256: crate::explore::content_hash(path)?,
    }))
}

fn frontmatter_value(raw: &str, key: &str) -> Option<String> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let needle = format!("{key}:");
    for line in rest[..end].lines() {
        let Some(value) = line.strip_prefix(&needle) else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn rel_path(root: &Path, path: &Path) -> anyhow::Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| format!("strip root from {}", path.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}

impl ItemKind {
    fn cmp_key(self) -> u8 {
        match self {
            ItemKind::Skill => 0,
            ItemKind::Agent => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> CatalogEntry {
        CatalogEntry {
            kind: ItemKind::Skill,
            name: "api-design".to_string(),
            source: "acme/repo".to_string(),
            rel_path: "skills/api-design".to_string(),
            pinned_commit: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            description: "REST heuristics".to_string(),
            license: Some("MIT".to_string()),
            content_sha256: "0".repeat(64),
        }
    }

    #[test]
    fn content_hash_file_and_dir_are_stable() {
        let td = tempfile::tempdir().unwrap();
        let f = td.path().join("SKILL.md");
        std::fs::write(&f, "hello").unwrap();
        let h1 = crate::explore::content_hash(&f).unwrap();
        assert_eq!(h1, crate::explore::content_hash(&f).unwrap());

        let d = td.path().join("skill");
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(d.join("SKILL.md"), "body").unwrap();
        std::fs::write(d.join("scripts/run.sh"), "#!/bin/sh").unwrap();
        let hd = crate::explore::content_hash(&d).unwrap();
        assert_eq!(hd, crate::explore::content_hash(&d).unwrap());
        assert_ne!(h1, hd);
    }

    #[test]
    fn catalog_saves_and_loads_from_index_path() {
        let td = tempfile::tempdir().unwrap();
        let cat = Catalog {
            synced_at_epoch: Some(1),
            entries: vec![sample_entry()],
        };

        cat.save(td.path()).unwrap();

        assert!(td.path().join(".spur/explore/index/catalog.json").exists());
        assert_eq!(Catalog::load(td.path()).unwrap().entries, cat.entries);
    }

    #[test]
    fn scan_finds_skills_and_agents_in_checkout() {
        let td = tempfile::tempdir().unwrap();
        let sk = td.path().join("skills/api-design");
        std::fs::create_dir_all(&sk).unwrap();
        std::fs::write(
            sk.join("SKILL.md"),
            "---\nname: api-design\ndescription: \"REST heuristics\"\nlicense: MIT\n---\nbody",
        )
        .unwrap();
        let ag = td.path().join("agents");
        std::fs::create_dir_all(&ag).unwrap();
        std::fs::write(
            ag.join("rust-pro.md"),
            "---\nname: rust-pro\ndescription: Rust specialist\n---\nYou are...",
        )
        .unwrap();
        std::fs::write(td.path().join("README.md"), "# readme").unwrap();

        let entries = scan_source_checkout(td.path(), "acme/repo", &"a".repeat(40)).unwrap();

        let names: Vec<_> = entries.iter().map(|e| (e.kind, e.name.as_str())).collect();
        assert!(names.contains(&(ItemKind::Skill, "api-design")));
        assert!(names.contains(&(ItemKind::Agent, "rust-pro")));
        assert_eq!(entries.len(), 2);
        let skill = entries.iter().find(|e| e.kind == ItemKind::Skill).unwrap();
        assert_eq!(skill.license.as_deref(), Some("MIT"));
        assert_eq!(skill.rel_path, "skills/api-design");
    }
}
