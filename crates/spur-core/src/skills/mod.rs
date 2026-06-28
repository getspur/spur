//! Skill-based brain prompt resources (Amendment A1).
//!
//! Loads SKILL.md files for brain prompt assembly. Bundled defaults are
//! filesystem assets; per-project overrides in `.spur/skills/` take
//! precedence.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod frontmatter;

pub mod adapters;
pub mod installer;

const SPUR_SKILLS_DIR_ENV: &str = "SPUR_SKILLS_DIR";
const CLAUDE_CODE_ACP_SKILL: &str = "brain-delegation-claude-code-acp";
const BUNDLED_ALIASES: &[(&str, &str)] = &[("brain-delegation-claude-code", CLAUDE_CODE_ACP_SKILL)];

static WORKSPACE_BUNDLED_RAW: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BundledRootSource {
    Env,
    Config,
    Package,
    Workspace,
}

impl BundledRootSource {
    fn label(self) -> &'static str {
        match self {
            BundledRootSource::Env => SPUR_SKILLS_DIR_ENV,
            BundledRootSource::Config => "[skills].bundled_dir",
            BundledRootSource::Package => "package asset path",
            BundledRootSource::Workspace => "workspace assets/skills",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedBundledRoot {
    path: PathBuf,
    source: BundledRootSource,
}

#[derive(Debug, Clone)]
struct RootCandidate {
    path: PathBuf,
    source: BundledRootSource,
}

impl RootCandidate {
    fn new(path: PathBuf, source: BundledRootSource) -> Self {
        Self { path, source }
    }

    fn render(&self) -> String {
        format!("{}={}", self.source.label(), self.path.display())
    }
}

/// Filesystem-backed catalog of bundled skill assets.
#[derive(Debug, Clone)]
pub struct SkillCatalog {
    bundled_root: PathBuf,
    source: BundledRootSource,
}

/// Error returned while resolving or reading bundled skill assets.
#[derive(Debug, thiserror::Error)]
pub enum SkillCatalogError {
    #[error(
        "missing bundled skill asset root; checked: {checked}; set SPUR_SKILLS_DIR or [skills].bundled_dir to a directory containing skill assets"
    )]
    MissingBundledRoot { checked: String },

    #[error("failed to load SPUR config while resolving bundled skills: {source}")]
    Config {
        #[source]
        source: anyhow::Error,
    },

    #[error("failed to read bundled skill asset root {path}: {source}")]
    ReadRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read bundled skill `{id}` from {path}: {source}")]
    ReadSkill {
        id: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid skill id `{id}`: {reason}")]
    InvalidSkillId { id: String, reason: String },
}

impl From<InvalidSkillId> for SkillCatalogError {
    fn from(e: InvalidSkillId) -> Self {
        SkillCatalogError::InvalidSkillId {
            id: e.id,
            reason: e.reason.to_string(),
        }
    }
}

impl SkillCatalog {
    pub fn discover(repo_root: &Path) -> Result<Self, SkillCatalogError> {
        let env_dir = std::env::var_os(SPUR_SKILLS_DIR_ENV).and_then(|value| {
            if value.is_empty() {
                None
            } else {
                Some(PathBuf::from(value))
            }
        });
        let config_dir = configured_bundled_dir(repo_root)?;
        let selected =
            select_bundled_root(repo_root, env_dir, config_dir, package_asset_candidates())?;
        Ok(Self {
            bundled_root: selected.path,
            source: selected.source,
        })
    }

    fn from_root(root: PathBuf, source: BundledRootSource) -> Self {
        Self {
            bundled_root: root,
            source,
        }
    }

    pub fn bundled_root(&self) -> &Path {
        &self.bundled_root
    }

    pub fn source(&self) -> &'static str {
        self.source.label()
    }

    pub fn load_raw(
        &self,
        id: &str,
        repo_root: &Path,
    ) -> Result<Option<String>, SkillCatalogError> {
        if let Some(raw) = read_valid_override(repo_root, id) {
            return Ok(Some(raw));
        }
        self.load_bundled_raw(id)
    }

    fn load_bundled_raw(&self, id: &str) -> Result<Option<String>, SkillCatalogError> {
        validate_id(id)?;
        let canonical = canonical_bundled_id(id);
        let path = self.bundled_root.join(canonical).join("SKILL.md");
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(Some(raw)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SkillCatalogError::ReadSkill {
                id: canonical.to_string(),
                path,
                source,
            }),
        }
    }

    pub fn list_raw(&self, _repo_root: &Path) -> Result<Vec<(String, String)>, SkillCatalogError> {
        Ok(self.list_bundled_raw_map()?.into_iter().collect())
    }

    fn list_bundled_raw_map(&self) -> Result<BTreeMap<String, String>, SkillCatalogError> {
        let entries = std::fs::read_dir(&self.bundled_root).map_err(|source| {
            SkillCatalogError::ReadRoot {
                path: self.bundled_root.clone(),
                source,
            }
        })?;
        let mut by_id = BTreeMap::new();
        for entry in entries {
            let entry = entry.map_err(|source| SkillCatalogError::ReadRoot {
                path: self.bundled_root.clone(),
                source,
            })?;
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            validate_id(&id)?;
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let raw = std::fs::read_to_string(&skill_md).map_err(|source| {
                SkillCatalogError::ReadSkill {
                    id: id.clone(),
                    path: skill_md,
                    source,
                }
            })?;
            by_id.insert(id, raw);
        }
        add_bundled_aliases(&mut by_id);
        Ok(by_id)
    }
}

fn configured_bundled_dir(repo_root: &Path) -> Result<Option<PathBuf>, SkillCatalogError> {
    let config = spur_acp::config::load_layered(repo_root)
        .map_err(|source| SkillCatalogError::Config { source })?;
    Ok(config
        .skills
        .bundled_dir
        .map(|path| absolutize_config_path(repo_root, path)))
}

fn absolutize_config_path(repo_root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

fn canonical_bundled_id(id: &str) -> &str {
    BUNDLED_ALIASES
        .iter()
        .find_map(|(alias, target)| (*alias == id).then_some(*target))
        .unwrap_or(id)
}

fn add_bundled_aliases(by_id: &mut BTreeMap<String, String>) {
    for (alias, target) in BUNDLED_ALIASES {
        if by_id.contains_key(*alias) {
            continue;
        }
        if let Some(raw) = by_id.get(*target).cloned() {
            by_id.insert((*alias).to_string(), raw);
        }
    }
}

fn select_bundled_root(
    repo_root: &Path,
    env_dir: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    package_candidates: Vec<PathBuf>,
) -> Result<SelectedBundledRoot, SkillCatalogError> {
    if let Some(path) = env_dir {
        return require_existing_root(vec![RootCandidate::new(path, BundledRootSource::Env)]);
    }
    if let Some(path) = config_dir {
        return require_existing_root(vec![RootCandidate::new(path, BundledRootSource::Config)]);
    }

    let mut candidates: Vec<RootCandidate> = package_candidates
        .into_iter()
        .map(|path| RootCandidate::new(path, BundledRootSource::Package))
        .collect();
    candidates.push(RootCandidate::new(
        repo_root.join("assets/skills"),
        BundledRootSource::Workspace,
    ));
    let manifest_root = manifest_workspace_asset_root();
    if !candidates
        .iter()
        .any(|candidate| candidate.path == manifest_root)
    {
        candidates.push(RootCandidate::new(
            manifest_root,
            BundledRootSource::Workspace,
        ));
    }
    require_existing_root(candidates)
}

#[cfg(test)]
fn resolve_bundled_root_for_test(
    repo_root: &Path,
    env_dir: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    package_candidates: Vec<PathBuf>,
) -> Result<SelectedBundledRoot, SkillCatalogError> {
    select_bundled_root(repo_root, env_dir, config_dir, package_candidates)
}

fn require_existing_root(
    candidates: Vec<RootCandidate>,
) -> Result<SelectedBundledRoot, SkillCatalogError> {
    for candidate in &candidates {
        if candidate.path.is_dir() {
            return Ok(SelectedBundledRoot {
                path: candidate.path.clone(),
                source: candidate.source,
            });
        }
    }
    Err(SkillCatalogError::MissingBundledRoot {
        checked: candidates
            .iter()
            .map(RootCandidate::render)
            .collect::<Vec<_>>()
            .join(", "),
    })
}

fn package_asset_candidates() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(bin_dir) = exe.parent() else {
        return Vec::new();
    };

    let mut candidates = vec![
        bin_dir.join("share/spur/skills"),
        bin_dir.join("assets/skills"),
    ];
    if let Some(prefix) = bin_dir.parent() {
        candidates.push(prefix.join("share/spur/skills"));
        candidates.push(prefix.join("assets/skills"));
    }
    candidates
}

fn manifest_workspace_asset_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/skills")
}

fn read_valid_override(repo_root: &Path, id: &str) -> Option<String> {
    validate_id(id).ok()?;
    let override_path = repo_root.join(".spur/skills").join(id).join("SKILL.md");
    if !override_path.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&override_path).ok()?;
    let parsed = frontmatter::parse_source(&raw);
    if is_unedited_spur_managed_source(&raw, parsed.body) || is_legacy_generated_spur_source(&raw) {
        return None;
    }
    Some(raw)
}

/// Returns all bundled skills (raw content including frontmatter) for CLI extraction.
pub fn all_bundled_raw() -> &'static HashMap<&'static str, &'static str> {
    WORKSPACE_BUNDLED_RAW.get_or_init(|| {
        let raw = SkillCatalog::from_root(
            manifest_workspace_asset_root(),
            BundledRootSource::Workspace,
        )
        .list_bundled_raw_map()
        .expect("workspace bundled skill assets must be readable");
        raw.into_iter()
            .map(|(id, body)| {
                let id = Box::leak(id.into_boxed_str()) as &'static str;
                let body = Box::leak(body.into_boxed_str()) as &'static str;
                (id, body)
            })
            .collect()
    })
}

/// Load a skill body: user override wins, else bundled default.
/// Frontmatter is stripped in both cases.
pub fn load_skill(name: &str, repo_root: &Path) -> Option<String> {
    if let Some(raw) = read_valid_override(repo_root, name) {
        return Some(frontmatter::parse_source(&raw).body.to_string());
    }
    let catalog = SkillCatalog::discover(repo_root).ok()?;
    catalog
        .load_bundled_raw(name)
        .ok()
        .flatten()
        .map(|raw| frontmatter::parse_source(&raw).body.to_string())
}

/// Strip YAML frontmatter delimited by `---\n...\n---\n`.
#[cfg(test)]
fn strip_frontmatter(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---\n") {
            return &rest[idx + 5..];
        }
        // Closing `---` at EOF (no trailing newline).
        if rest.ends_with("\n---") {
            return "";
        }
    }
    s
}

static SKILL_ID_RE: OnceLock<regex::Regex> = OnceLock::new();

fn skill_id_regex() -> &'static regex::Regex {
    SKILL_ID_RE
        .get_or_init(|| regex::Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").expect("static regex"))
}

/// Error returned when a skill directory name violates the naming rules.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid skill id `{id}`: {reason}")]
pub struct InvalidSkillId {
    pub id: String,
    pub reason: &'static str,
}

/// Validate a skill id: regex `^[a-z0-9]+(-[a-z0-9]+)*$`, length 1..=54.
///
/// The 54-char cap is OpenCode's 64-char skill-name limit minus the
/// `spurpower-` 10-char prefix we add in adapter output.
pub fn validate_id(id: &str) -> Result<(), InvalidSkillId> {
    if id.is_empty() {
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "empty",
        });
    }
    if id.len() > 54 {
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "longer than 54 characters",
        });
    }
    if !skill_id_regex().is_match(id) {
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "must match ^[a-z0-9]+(-[a-z0-9]+)*$",
        });
    }
    Ok(())
}

/// Where a skill's body came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    Bundled,
    Override,
}

/// Which agent role a skill targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillRole {
    /// Injected into brain prompts; not rendered to worker agent adapters.
    Brain,
    /// Rendered to worker agent adapters; not injected into brain prompts.
    Worker,
    /// Both brain and worker contexts.
    #[default]
    Both,
}

impl std::str::FromStr for SkillRole {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "brain" => Ok(SkillRole::Brain),
            "worker" => Ok(SkillRole::Worker),
            "both" => Ok(SkillRole::Both),
            _ => Err(()),
        }
    }
}

/// A resolved skill ready for rendering across adapters.
#[derive(Debug, Clone)]
pub struct SkillPayload {
    pub id: String,
    pub description: String,
    pub body: String,
    pub source: SkillSource,
    pub role: SkillRole,
}

/// Resolve the active skill set: bundled corpus merged with
/// `.spur/skills/<id>/SKILL.md` overrides (override wins per id).
///
/// Validates every skill id (bundled and override) through `validate_id`.
///
/// Returns `SkillCatalogError` instead of only `InvalidSkillId` because
/// filesystem-backed assets can also fail during root discovery or reads.
pub fn list_active_skills(repo_root: &Path) -> Result<Vec<SkillPayload>, SkillCatalogError> {
    let mut by_id: std::collections::BTreeMap<String, SkillPayload> =
        std::collections::BTreeMap::new();

    // Bundled first.
    let catalog = SkillCatalog::discover(repo_root)?;
    for (id, raw) in catalog.list_raw(repo_root)? {
        validate_id(&id)?;
        let parsed = frontmatter::parse_source(&raw);
        by_id.insert(
            id.clone(),
            SkillPayload {
                id,
                description: parsed.description.as_deref().unwrap_or("").to_string(),
                body: parsed.body.to_string(),
                source: SkillSource::Bundled,
                role: parsed.role.unwrap_or(SkillRole::Both),
            },
        );
    }

    // Overrides.
    let override_dir = repo_root.join(".spur/skills");
    if override_dir.is_dir() {
        let entries = match std::fs::read_dir(&override_dir) {
            Ok(e) => e,
            Err(_) => return Ok(by_id.into_values().collect()),
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            validate_id(&id)?;
            let skill_md = entry.path().join("SKILL.md");
            let raw = match std::fs::read_to_string(&skill_md) {
                Ok(r) => r,
                Err(_) => continue, // no SKILL.md in that dir
            };
            let parsed = frontmatter::parse_source(&raw);
            if is_unedited_spur_managed_source(&raw, parsed.body)
                || is_legacy_generated_spur_source(&raw)
            {
                continue;
            }
            by_id.insert(
                id.clone(),
                SkillPayload {
                    id,
                    description: parsed.description.as_deref().unwrap_or("").to_string(),
                    body: parsed.body.to_string(),
                    source: SkillSource::Override,
                    role: parsed.role.unwrap_or(SkillRole::Both),
                },
            );
        }
    }

    Ok(by_id.into_values().collect())
}

fn is_unedited_spur_managed_source(raw: &str, body: &str) -> bool {
    raw.lines()
        .filter_map(|line| installer::parse_marker(line.trim()))
        .any(|marker| installer::sha256_hex(body.as_bytes()) == marker.sha256)
}

fn is_legacy_generated_spur_source(raw: &str) -> bool {
    raw.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line == "<!-- GENERATED BY SPUR. DO NOT EDIT. -->")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    fn write_skill(root: &Path, id: &str, description: &str, body: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    fn workspace_asset_skill_dir(id: &str) -> PathBuf {
        manifest_workspace_asset_root().join(id)
    }

    fn toml_basic_string(path: &Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }

    #[test]
    fn bundled_skills_parse_and_strip_frontmatter() {
        let map = all_bundled_raw();
        for name in [
            "brain-delegation",
            "brain-delegation-claude-code-acp",
            "brain-delegation-kiro",
            "brain-delegation-codex",
            "brain-delegation-gemini",
            "test-driven-development",
            "systematic-debugging",
            "verification-before-completion",
            "receiving-code-review",
            "requesting-code-review",
            "spur-way",
            "beads-lifecycle",
            "worker-signals",
            "brain-review-gate",
            "plan-task-discipline",
            "worker-mention-routing",
            "writing-skills",
            "code-explore",
            "spur-analyst",
        ] {
            let raw = map
                .get(name)
                .unwrap_or_else(|| panic!("missing bundled skill: {name}"));
            let body = frontmatter::parse_source(raw).body;
            assert!(!body.starts_with("---"), "{name}: frontmatter not stripped");
            assert!(!body.is_empty(), "{name}: body is empty after strip");
        }
    }

    #[test]
    fn load_skill_returns_bundled_when_no_override() {
        let fake_root = PathBuf::from("/nonexistent-spur-test-root");
        let body = load_skill("brain-delegation", &fake_root);
        assert!(body.is_some(), "expected bundled skill");
        assert!(
            body.unwrap().contains("delegate"),
            "expected delegation content"
        );
    }

    #[test]
    fn load_skill_returns_none_for_unknown() {
        let fake_root = PathBuf::from("/nonexistent-spur-test-root");
        assert!(load_skill("nonexistent-skill", &fake_root).is_none());
    }

    #[test]
    fn load_skill_reads_repo_assets_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let asset_root = dir.path().join("assets/skills");
        write_skill(
            &asset_root,
            "asset-only",
            "Asset fallback",
            "Asset fallback body\n",
        );

        let body = load_skill("asset-only", dir.path()).unwrap();

        assert_eq!(body, "Asset fallback body\n");
    }

    #[test]
    fn list_active_skills_reads_configured_bundled_dir() {
        let repo = tempfile::tempdir().unwrap();
        let asset_root = tempfile::tempdir().unwrap();
        write_skill(
            asset_root.path(),
            "configured-skill",
            "Configured bundled skill",
            "Configured body\n",
        );
        std::fs::create_dir_all(repo.path().join(".spur")).unwrap();
        std::fs::write(
            repo.path().join(".spur/config.toml"),
            format!(
                "[skills]\nbundled_dir = '{}'\n",
                asset_root.path().display()
            ),
        )
        .unwrap();

        let skills = list_active_skills(repo.path()).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "configured-skill");
        assert_eq!(skills[0].description, "Configured bundled skill");
        assert_eq!(skills[0].body, "Configured body\n");
        assert!(matches!(skills[0].source, SkillSource::Bundled));
    }

    #[test]
    fn list_active_skills_reports_missing_configured_bundled_dir() {
        let repo = tempfile::tempdir().unwrap();
        let missing = repo.path().join("missing-skills");
        std::fs::create_dir_all(repo.path().join(".spur")).unwrap();
        std::fs::write(
            repo.path().join(".spur/config.toml"),
            format!("[skills]\nbundled_dir = '{}'\n", missing.display()),
        )
        .unwrap();

        let err = list_active_skills(repo.path()).unwrap_err().to_string();

        assert!(err.contains(&missing.display().to_string()), "{err}");
        assert!(err.contains("[skills].bundled_dir"), "{err}");
        assert!(err.contains("SPUR_SKILLS_DIR"), "{err}");
    }

    #[test]
    fn load_skill_uses_override_when_configured_bundled_dir_is_missing() {
        let repo = tempfile::tempdir().unwrap();
        let missing = repo.path().join("missing-skills");
        std::fs::create_dir_all(repo.path().join(".spur/skills/override-only")).unwrap();
        std::fs::write(
            repo.path().join(".spur/config.toml"),
            format!("[skills]\nbundled_dir = '{}'\n", missing.display()),
        )
        .unwrap();
        std::fs::write(
            repo.path()
                .join(".spur/skills/override-only")
                .join("SKILL.md"),
            "---\nname: override-only\ndescription: Override\n---\nOverride body\n",
        )
        .unwrap();

        let body = load_skill("override-only", repo.path()).unwrap();

        assert_eq!(body, "Override body\n");
    }

    #[test]
    fn env_bundled_dir_wins_over_configured_dir() {
        let repo = tempfile::tempdir().unwrap();
        let env_root = tempfile::tempdir().unwrap();
        let config_root = tempfile::tempdir().unwrap();

        let selected = resolve_bundled_root_for_test(
            repo.path(),
            Some(env_root.path().to_path_buf()),
            Some(config_root.path().to_path_buf()),
            Vec::new(),
        )
        .unwrap();

        assert_eq!(selected.path, env_root.path());
        assert_eq!(selected.source, BundledRootSource::Env);
    }

    #[test]
    fn env_bundled_dir_is_read_from_process_env_and_wins_over_config() {
        let repo = tempfile::tempdir().unwrap();
        let env_root = tempfile::tempdir().unwrap();
        let config_root = tempfile::tempdir().unwrap();
        write_skill(
            env_root.path(),
            "env-skill",
            "Env bundled skill",
            "Env body\n",
        );
        write_skill(
            config_root.path(),
            "config-skill",
            "Config bundled skill",
            "Config body\n",
        );
        std::fs::create_dir_all(repo.path().join(".spur")).unwrap();
        std::fs::write(
            repo.path().join(".spur/config.toml"),
            format!(
                "[skills]\nbundled_dir = \"{}\"\n",
                toml_basic_string(config_root.path())
            ),
        )
        .unwrap();

        let output = Command::new(std::env::current_exe().unwrap())
            .arg("skills::tests::env_bundled_dir_child_asserts_process_env")
            .arg("--exact")
            .arg("--nocapture")
            .env("SPUR_SKILLS_ENV_CHILD", "1")
            .env(SPUR_SKILLS_DIR_ENV, env_root.path())
            .env("SPUR_SKILLS_ENV_REPO", repo.path())
            .output()
            .expect("child test process should run");

        assert!(
            output.status.success(),
            "child env test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("1 passed"),
            "expected exactly one child test to run, got stdout:\n{stdout}"
        );
    }

    #[test]
    fn env_bundled_dir_child_asserts_process_env() {
        if std::env::var_os("SPUR_SKILLS_ENV_CHILD").is_none() {
            return;
        }
        let repo = PathBuf::from(std::env::var_os("SPUR_SKILLS_ENV_REPO").unwrap());

        let catalog = SkillCatalog::discover(&repo).unwrap();
        let skills = list_active_skills(&repo).unwrap();
        let ids = skills
            .iter()
            .map(|skill| skill.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(catalog.source(), SPUR_SKILLS_DIR_ENV);
        assert_eq!(ids, vec!["env-skill"]);
    }

    #[test]
    fn package_asset_candidates_cover_dist_included_assets_tree() {
        let exe = std::env::current_exe().unwrap();
        let bin_dir = exe.parent().unwrap();
        let candidates = package_asset_candidates();

        assert!(candidates.contains(&bin_dir.join("share/spur/skills")));
        assert!(candidates.contains(&bin_dir.join("assets/skills")));
        if let Some(prefix) = bin_dir.parent() {
            assert!(candidates.contains(&prefix.join("share/spur/skills")));
            assert!(candidates.contains(&prefix.join("assets/skills")));
        }
    }

    #[test]
    fn claude_code_alias_resolves_to_configured_acp_asset() {
        let repo = tempfile::tempdir().unwrap();
        let asset_root = tempfile::tempdir().unwrap();
        write_skill(
            asset_root.path(),
            "brain-delegation-claude-code-acp",
            "Configured Claude ACP skill",
            "Configured Claude ACP body\n",
        );
        std::fs::create_dir_all(repo.path().join(".spur")).unwrap();
        std::fs::write(
            repo.path().join(".spur/config.toml"),
            format!(
                "[skills]\nbundled_dir = '{}'\n",
                asset_root.path().display()
            ),
        )
        .unwrap();

        let acp = load_skill("brain-delegation-claude-code-acp", repo.path()).unwrap();
        let alias = load_skill("brain-delegation-claude-code", repo.path()).unwrap();

        assert_eq!(acp, "Configured Claude ACP body\n");
        assert_eq!(alias, acp);
    }

    #[test]
    fn load_skill_prefers_user_override() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".spur/skills/brain-delegation");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: brain-delegation\n---\nCustom override body\n",
        )
        .unwrap();
        let body = load_skill("brain-delegation", dir.path()).unwrap();
        assert_eq!(body.trim(), "Custom override body");
    }

    #[test]
    fn load_skill_ignores_unedited_spur_managed_override() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let stale_body = "Old generated TDD body\n";
        let marker = crate::skills::installer::Marker {
            version: 1,
            skill_id: "test-driven-development".to_string(),
            sha256: crate::skills::installer::sha256_hex(stale_body.as_bytes()),
        };
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: test-driven-development\ndescription: >\n---\n{marker}{stale_body}",
                marker = marker.render(),
            ),
        )
        .unwrap();

        let body = load_skill("test-driven-development", dir.path()).unwrap();

        assert!(!body.contains(stale_body));
        assert!(body.contains("# Test-Driven Development"));
    }

    #[test]
    fn load_skill_ignores_legacy_generated_override() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "<!-- GENERATED BY SPUR. DO NOT EDIT. -->\n\n---\nname: test-driven-development\ndescription: old\n---\nOld legacy body\n",
        )
        .unwrap();

        let body = load_skill("test-driven-development", dir.path()).unwrap();

        assert!(!body.contains("Old legacy body"));
        assert!(body.contains("# Test-Driven Development"));
    }

    #[test]
    fn strip_frontmatter_no_frontmatter() {
        assert_eq!(strip_frontmatter("just body"), "just body");
    }

    #[test]
    fn strip_frontmatter_normal() {
        let input = "---\nfoo: bar\n---\nbody text\n";
        assert_eq!(strip_frontmatter(input), "body text\n");
    }

    #[test]
    fn strip_frontmatter_eof_no_trailing_newline() {
        let input = "---\nfoo: bar\n---";
        assert_eq!(strip_frontmatter(input), "");
    }

    #[test]
    fn strip_frontmatter_value_containing_dashes() {
        // YAML value with --- should not be treated as closing delimiter.
        let input = "---\nfoo: some---value\n---\nbody\n";
        assert_eq!(strip_frontmatter(input), "body\n");
    }

    #[test]
    fn claude_code_deprecated_alias_resolves() {
        let fake = PathBuf::from("/nonexistent");
        let acp = load_skill("brain-delegation-claude-code-acp", &fake).unwrap();
        let alias = load_skill("brain-delegation-claude-code", &fake).unwrap();
        assert_eq!(acp, alias);
    }

    #[test]
    fn validate_id_accepts_standard_names() {
        for ok in [
            "tdd",
            "test-driven-development",
            "a",
            "verification-before-completion",
        ] {
            assert!(validate_id(ok).is_ok(), "should accept {ok}");
        }
    }

    #[test]
    fn validate_id_rejects_bad_names() {
        for bad in [
            "",
            "Uppercase",
            "has space",
            "has_underscore",
            "trailing-",
            "-leading",
            "double--hyphen",
            "with/slash",
            "..",
            "../evil",
        ] {
            assert!(validate_id(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn validate_id_enforces_length_cap() {
        let ok_54 = "a".to_string() + &"b".repeat(53);
        let too_long_55 = "a".to_string() + &"b".repeat(54);
        assert!(validate_id(&ok_54).is_ok());
        assert!(validate_id(&too_long_55).is_err());
    }

    #[test]
    fn list_active_skills_returns_bundled_when_no_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let skills = list_active_skills(dir.path()).unwrap();
        let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"test-driven-development"));
        assert!(ids.contains(&"brain-delegation"));
        assert!(
            ids.contains(&"spur-way"),
            "spur-way skill should be bundled"
        );
        assert!(
            ids.contains(&"beads-lifecycle"),
            "beads-lifecycle skill should be bundled"
        );
        assert!(
            ids.contains(&"worker-signals"),
            "worker-signals skill should be bundled"
        );
        assert!(
            ids.contains(&"worker-mention-routing"),
            "worker-mention-routing skill should be bundled"
        );
        // All bundled entries should have non-empty body.
        for s in &skills {
            assert!(!s.body.is_empty(), "{}: empty body", s.id);
        }
    }

    #[test]
    fn list_active_skills_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("SKILL.md"),
            "---\nname: test-driven-development\ndescription: MY OVERRIDE\n---\nMy body here\n",
        )
        .unwrap();

        let skills = list_active_skills(dir.path()).unwrap();
        let tdd = skills
            .iter()
            .find(|s| s.id == "test-driven-development")
            .unwrap();
        assert_eq!(tdd.description, "MY OVERRIDE");
        assert_eq!(tdd.body, "My body here\n");
        assert!(matches!(tdd.source, SkillSource::Override));
    }

    #[test]
    fn list_active_skills_ignores_unedited_spur_managed_skill_files() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&override_dir).unwrap();
        let body = "# Test-Driven Development (TDD)\n\nOld generated body\n";
        let marker = crate::skills::installer::Marker {
            version: 1,
            skill_id: "test-driven-development".to_string(),
            sha256: crate::skills::installer::sha256_hex(body.as_bytes()),
        };
        std::fs::write(
            override_dir.join("SKILL.md"),
            format!(
                "---\nname: test-driven-development\ndescription: >\nrole: both\n---\n{marker}{body}",
                marker = marker.render(),
            ),
        )
        .unwrap();

        let skills = list_active_skills(dir.path()).unwrap();
        let tdd = skills
            .iter()
            .find(|s| s.id == "test-driven-development")
            .unwrap();
        assert!(matches!(tdd.source, SkillSource::Bundled));
        assert_eq!(tdd.role, SkillRole::Worker);
        assert_eq!(
            tdd.description,
            "Use when implementing any feature or bugfix, before writing implementation code"
        );
    }

    #[test]
    fn list_active_skills_rejects_invalid_override_id() {
        let dir = tempfile::tempdir().unwrap();
        let bad_override = dir.path().join(".spur/skills/Bad_Name");
        std::fs::create_dir_all(&bad_override).unwrap();
        std::fs::write(
            bad_override.join("SKILL.md"),
            "---\nname: bad\ndescription: x\n---\nbody",
        )
        .unwrap();

        let err = list_active_skills(dir.path()).unwrap_err();
        assert!(err.to_string().contains("Bad_Name"));
    }

    #[test]
    fn spur_way_skill_contains_beads_first_invariant() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("spur-way", &fake).unwrap();
        assert!(body.contains("beads is the sole source of truth"));
        assert!(body.contains("INTENT"));
        assert!(body.contains("ACTION"));
        assert!(body.contains("RECORD"));
    }

    #[test]
    fn beads_lifecycle_skill_contains_status_fsm() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("beads-lifecycle", &fake).unwrap();
        assert!(body.contains("open"));
        assert!(body.contains("in_progress"));
        assert!(body.contains("signal:"));
    }

    #[test]
    fn worker_signals_skill_contains_exact_format() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("worker-signals", &fake).unwrap();
        assert!(body.contains("[[spur-signal v1]]"));
        assert!(body.contains("signal_id"));
        assert!(body.contains("severity"));
    }

    #[test]
    fn brain_review_gate_skill_contains_checklist() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("brain-review-gate", &fake).unwrap();
        assert!(body.contains("NO APPROVAL WITHOUT BEADS VERIFICATION"));
        assert!(body.contains("Audit Trail Check"));
    }

    #[test]
    fn plan_task_discipline_skill_contains_dag_rules() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("plan-task-discipline", &fake).unwrap();
        assert!(body.contains("DAG"));
        assert!(body.contains("Pending"));
        assert!(body.contains("Approved"));
    }

    #[test]
    fn worker_mention_routing_skill_contains_hierarchy() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("worker-mention-routing", &fake).unwrap();
        assert!(
            body.contains("User @mention outranks your algorithm"),
            "should declare user intent supremacy"
        );
        assert!(
            body.contains("list_available_workers"),
            "should require validation"
        );
        assert!(
            body.contains("avoid_for"),
            "should reference avoid_for override condition"
        );
    }

    #[test]
    fn brainstorming_skill_contains_beads_epic_creation() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("brainstorming", &fake).unwrap();
        assert!(
            body.contains("create_issue"),
            "should instruct creating beads epic"
        );
        assert!(
            body.contains("NO IMPLEMENTATION WITHOUT AN APPROVED SPEC AND A BEADS EPIC"),
            "should enforce beads-first design gate"
        );
        assert!(
            body.contains("Invoke writing-plans"),
            "should hand off to writing-plans"
        );
    }

    #[test]
    fn writing_plans_skill_contains_dag_and_beads_integration() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("writing-plans", &fake).unwrap();
        assert!(
            body.contains("submit_plan"),
            "should reference plan submission"
        );
        assert!(
            body.contains("Depends on:"),
            "should define task dependencies"
        );
        assert!(
            body.contains("spur:plan-task-id"),
            "should reference beads plan task labels"
        );
        assert!(
            body.contains("Scope Boundary:"),
            "should define worker scope boundaries"
        );
    }

    #[test]
    fn code_explore_skill_establishes_three_layer_stack() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("code-explore", &fake).unwrap();
        assert!(
            body.contains("knowledge_context_pack_2"),
            "layer 1: skill must establish knowledge_context_pack_2 as the orientation entry point"
        );
        assert!(
            body.contains("spur-analyst"),
            "layer 3: skill must route aggregation/graph-algorithm questions to spur-analyst"
        );
        assert!(
            body.contains("recommended_next_tools"),
            "skill must teach the selector hand-off from the pack into code_* tools"
        );
        assert!(
            body.contains("calibrated"),
            "skill must document calibrated pack confidence"
        );
        assert!(
            body.contains("label_inbound") && body.contains("inbound_unresolved"),
            "skill must document resolved-vs-label caller evidence"
        );
        assert!(
            body.contains("calls_dyn") && body.contains("references_hof"),
            "skill must document dynamic/HOF path edge coverage"
        );
        assert!(
            body.contains("supporting_docs"),
            "skill must teach judging code recall against the doc hits"
        );
    }

    #[test]
    fn code_explore_skill_documents_external_package_tools() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("code-explore", &fake).unwrap();
        for keyword in [
            "external_knowledge_context",
            "external_code_search",
            "external_code_read",
            "external_code_callers",
            "external_code_callees",
            "external_index",
            "external_index_status",
            "pkg:serde@1.0.197::Deserialize",
        ] {
            assert!(
                body.contains(keyword),
                "code-explore must document external package MCP support via `{keyword}`"
            );
        }
    }

    #[test]
    fn code_explore_description_names_all_three_layers() {
        let raw = all_bundled_raw().get("code-explore").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("");
        for keyword in ["knowledge_context_pack_2", "code_*", "spur-analyst"] {
            assert!(
                desc.contains(keyword),
                "description must carry `{keyword}` so agents discover the skill for that layer, got: {desc}"
            );
        }
    }

    #[test]
    fn spur_analyst_skill_is_bundled_and_loadable() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("spur-analyst", &fake).expect("spur-analyst skill must be bundled");
        assert!(
            body.contains("DuckPGQ"),
            "skill must document the property-graph extension"
        );
        assert!(
            body.contains("information_schema.columns"),
            "skill must keep the schema-discovery hard gate"
        );
        assert!(
            body.contains("code_*"),
            "skill must keep the layer-2 routing table"
        );
    }

    #[test]
    fn brainstorming_description_contains_trigger_phrases() {
        let raw = all_bundled_raw().get("brainstorming").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("");
        assert!(
            desc.contains("brainstorm") || desc.contains("design"),
            "description should contain trigger phrases for matching, got: {desc}"
        );
    }

    #[test]
    fn writing_plans_description_contains_trigger_phrases() {
        let raw = all_bundled_raw().get("writing-plans").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("");
        assert!(
            desc.contains("plan") || desc.contains("tasks"),
            "description should contain trigger phrases for matching, got: {desc}"
        );
    }

    #[test]
    fn open_design_description_contains_trigger_phrases() {
        let raw = all_bundled_raw().get("open-design").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("").to_lowercase();
        assert!(
            desc.contains("design") || desc.contains("landing") || desc.contains("deck"),
            "description should contain visual-design trigger phrases, got: {desc}"
        );
        assert_eq!(
            parsed.role,
            Some(crate::skills::SkillRole::Brain),
            "open-design is a brain-role skill"
        );
    }

    #[test]
    fn open_design_skill_is_bundled_and_loadable() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).expect("open-design skill must be bundled");
        // Drives the notebook through the MCP tool surface, not a Node daemon.
        assert!(
            body.contains("notebook_insert_cell"),
            "skill must instruct driving the notebook via notebook_* tools"
        );
        assert!(
            body.contains("text/html"),
            "skill must instruct emitting the artifact as a text/html cell output"
        );
    }

    #[test]
    fn open_design_skill_covers_full_loop() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        for marker in [
            "Discovery", // brief-lock step
            "Direction", // direction picker step
            "Plan",      // todo plan step
            "Artifact",  // artifact emission step
            "Critique",  // self-critique step
            "references/directions.md",
            "references/critique.md",
            "notebook_read_cell",
            "notebook_write_cell",
        ] {
            assert!(
                body.contains(marker),
                "open-design body must cover `{marker}`"
            );
        }
    }

    #[test]
    fn open_design_deck_mode_native_flow() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        assert!(
            body.contains("references/deck-mode.md"),
            "Artifact step must route kind:deck to the native deck guide"
        );
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/skills/open-design/references/deck-mode.md");
        let text = std::fs::read_to_string(&refs).expect("deck-mode.md must exist");
        for marker in [
            "jute_deck",
            "set_cell_metadata",
            "title",
            "section",
            "bullets",
            "speaker_notes",
        ] {
            assert!(
                text.contains(marker),
                "deck-mode.md must document `{marker}`"
            );
        }
    }

    #[test]
    fn open_design_deck_mode_lists_ported_themes() {
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/skills/open-design/references/deck-mode.md");
        let text = std::fs::read_to_string(&refs).expect("deck-mode.md must exist");
        for id in [
            "editorial-monocle",
            "modern-minimal",
            "warm-soft",
            "tech-utility",
            "brutalist",
        ] {
            assert!(
                text.contains(id),
                "deck-mode.md must list ported theme `{id}`"
            );
        }
    }

    #[test]
    fn open_design_deck_artifact_track() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        assert!(
            body.contains("references/deck-artifact.md"),
            "SKILL.md must route polished/branded decks to the artifact track"
        );
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/skills/open-design/references/deck-artifact.md");
        let text = std::fs::read_to_string(&refs).expect("deck-artifact.md must exist");
        for marker in [
            "deck-skeleton.html",
            "index.json",
            "open_design_search",
            "open_design_get",
            "text/html",
            "SLOT:",
            "native",
        ] {
            assert!(
                text.contains(marker),
                "deck-artifact.md must document `{marker}`"
            );
        }
    }

    #[test]
    fn open_design_critique_has_deck_checks() {
        let dir = workspace_asset_skill_dir("open-design").join("references");
        let critique = std::fs::read_to_string(dir.join("critique.md")).unwrap();
        assert!(
            critique.contains("Deck-specific checks"),
            "critique.md must include deck-specific checks"
        );
        for marker in ["one idea per slide", "theme rhythm", "slide counter"] {
            assert!(
                critique.contains(marker),
                "deck checks must cover `{marker}`"
            );
        }
    }

    #[test]
    fn open_design_critique_has_artifact_deck_checks() {
        let dir = workspace_asset_skill_dir("open-design").join("references");
        let critique = std::fs::read_to_string(dir.join("critique.md")).unwrap();
        assert!(
            critique.contains("Artifact-deck checks"),
            "critique.md must include artifact-deck checks"
        );
        for marker in ["scale-to-fit", "slot", "16:9", "verbatim framework"] {
            assert!(
                critique.contains(marker),
                "artifact-deck checks must cover `{marker}`"
            );
        }
    }

    #[test]
    fn open_design_references_design_system_library() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        assert!(
            body.contains("references/design-systems.md"),
            "Direction step must point at the design-system library reference"
        );
        // The reference doc itself ships beside the skill source.
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/skills/open-design/references/design-systems.md");
        let text = std::fs::read_to_string(&refs).expect("design-systems.md must exist");
        assert!(
            text.contains("index.json") && text.contains("swatches"),
            "reference must document the index schema"
        );
        for marker in ["open_design_search", "open_design_get"] {
            assert!(
                text.contains(marker),
                "design-systems.md must document `{marker}`"
            );
        }
    }

    #[test]
    fn open_design_directions_reference_lists_all_five() {
        // The reference files live beside the bundled skill source.
        let dir = workspace_asset_skill_dir("open-design").join("references");
        let directions =
            std::fs::read_to_string(dir.join("directions.md")).expect("directions.md must exist");
        for school in [
            "Editorial Monocle",
            "Modern Minimal",
            "Warm Soft",
            "Tech Utility",
            "Brutalist Experimental",
        ] {
            assert!(
                directions.contains(school),
                "directions.md must list `{school}`"
            );
        }
        assert!(
            directions.contains("oklch"),
            "directions must carry deterministic OKLch palettes"
        );
        let critique =
            std::fs::read_to_string(dir.join("critique.md")).expect("critique.md must exist");
        assert!(
            critique.to_lowercase().contains("anti-ai-slop")
                || critique.to_lowercase().contains("anti ai slop"),
            "critique.md must include the anti-AI-slop checklist"
        );
    }
}
