use crate::explore::catalog::ItemKind;
use crate::explore::pool::Manifest;
use crate::skills::adapters::Adapter;
use crate::skills::{SkillCatalog, SkillPayload, SkillRole, SkillSource, SkillSourceCandidate};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CATALOG_BOOTSTRAP_ID: &str = "skills-catalog";

/// Origin selected for one effective skill ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedSourceKind {
    Bundled,
    Pool,
    RepositoryOverride,
}

/// Content-addressed identity of a selected skill source directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedSource {
    pub kind: ResolvedSourceKind,
    pub content_sha256: String,
    pub source_dir: PathBuf,
}

/// Skill payload paired with the exact source directory used to render it.
#[derive(Debug, Clone)]
pub struct ResolvedSkill {
    pub payload: SkillPayload,
    pub source: ResolvedSource,
}

/// Failure while loading and merging the effective skill set.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error(transparent)]
    Catalog(#[from] crate::skills::SkillCatalogError),
    #[error(transparent)]
    InvalidId(#[from] crate::skills::InvalidSkillId),
    #[error("failed to load layered pool manifest: {0}")]
    Manifest(#[source] anyhow::Error),
    #[error("pool skill {id} digest mismatch: expected {expected}, actual {actual}")]
    PoolDigestMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("pool skill {id} collides with bundled without replaced-bundled verdict")]
    PoolReplacementNotAuthorized { id: String },
    #[error("bundled catalog bootstrap is missing at {path}")]
    CatalogBootstrapMissing { path: PathBuf },
    #[error("bundled catalog bootstrap at {path} failed integrity validation: {reason}")]
    CatalogBootstrapIntegrity { path: PathBuf, reason: &'static str },
    #[error("failed to read skill source {path}: {source}")]
    ReadSource {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Resolve the policy-selected skill set for one adapter in canonical ID order.
pub fn resolve_effective_skills(
    repo_root: &Path,
    adapter: Adapter,
    _role: super::RuntimeRole,
    policy: super::SelectionPolicy,
) -> Result<Vec<ResolvedSkill>, ResolveError> {
    match policy {
        super::SelectionPolicy::AllActive => {}
        super::SelectionPolicy::CatalogOnly => return resolve_catalog_bootstrap(repo_root),
    }

    let mut by_id = BTreeMap::new();
    for candidate in crate::skills::bundled_skill_candidates(repo_root)? {
        let resolved = resolve_candidate(candidate, ResolvedSourceKind::Bundled)?;
        by_id.insert(resolved.payload.id.clone(), resolved);
    }
    let bundled_ids = by_id.keys().cloned().collect::<BTreeSet<_>>();

    let manifest = Manifest::load_layered(repo_root).map_err(ResolveError::Manifest)?;
    for item in manifest
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Skill)
    {
        crate::skills::validate_id(&item.name)?;
        if !matches!(
            item.gate.verdict.as_str(),
            "clean" | "overridden" | "replaced-bundled"
        ) {
            return Err(ResolveError::Manifest(anyhow::anyhow!(
                "pool skill {} has non-materializable gate verdict `{}`",
                item.name,
                item.gate.verdict
            )));
        }
        if bundled_ids.contains(&item.name) && item.gate.verdict != "replaced-bundled" {
            return Err(ResolveError::PoolReplacementNotAuthorized {
                id: item.name.clone(),
            });
        }

        let loaded = crate::explore::materialize::load_pool_skill(repo_root, item)
            .map_err(ResolveError::Manifest)?;
        let actual = hash_source_dir(&loaded.source_dir)?;
        if actual != item.content_sha256 {
            return Err(ResolveError::PoolDigestMismatch {
                id: item.name.clone(),
                expected: item.content_sha256.clone(),
                actual,
            });
        }
        let id = loaded.payload.id.clone();
        by_id.insert(
            id,
            ResolvedSkill {
                payload: loaded.payload,
                source: ResolvedSource {
                    kind: ResolvedSourceKind::Pool,
                    content_sha256: item.content_sha256.clone(),
                    source_dir: loaded.source_dir,
                },
            },
        );
    }

    for candidate in crate::skills::repository_override_candidates(repo_root)? {
        if candidate.payload.role == SkillRole::Brain && adapter != Adapter::SpurHermetic {
            continue;
        }
        let resolved = resolve_candidate(candidate, ResolvedSourceKind::RepositoryOverride)?;
        by_id.insert(resolved.payload.id.clone(), resolved);
    }

    Ok(by_id.into_values().collect())
}

fn resolve_catalog_bootstrap(repo_root: &Path) -> Result<Vec<ResolvedSkill>, ResolveError> {
    let catalog = SkillCatalog::discover(repo_root)?;
    let bundled_root = catalog.bundled_root();
    let source_dir = bundled_root.join(CATALOG_BOOTSTRAP_ID);
    let skill_path = source_dir.join("SKILL.md");

    let source_metadata = match std::fs::symlink_metadata(&source_dir) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(ResolveError::CatalogBootstrapMissing { path: skill_path });
        }
        Err(source) => {
            return Err(ResolveError::ReadSource {
                path: source_dir,
                source,
            });
        }
    };
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: source_dir,
            reason: "bootstrap source must be a real directory, not a symlink",
        });
    }

    let canonical_root =
        std::fs::canonicalize(bundled_root).map_err(|source| ResolveError::ReadSource {
            path: bundled_root.to_path_buf(),
            source,
        })?;
    let canonical_source =
        std::fs::canonicalize(&source_dir).map_err(|source| ResolveError::ReadSource {
            path: source_dir.clone(),
            source,
        })?;
    if canonical_source.parent() != Some(canonical_root.as_path()) {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: source_dir,
            reason: "bootstrap source must be a direct child of the bundled root",
        });
    }

    let skill_metadata = match std::fs::symlink_metadata(&skill_path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(ResolveError::CatalogBootstrapMissing { path: skill_path });
        }
        Err(source) => {
            return Err(ResolveError::ReadSource {
                path: skill_path,
                source,
            });
        }
    };
    if skill_metadata.file_type().is_symlink() || !skill_metadata.is_file() {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: skill_path,
            reason: "bootstrap SKILL.md must be a real regular file, not a symlink",
        });
    }
    let canonical_skill =
        std::fs::canonicalize(&skill_path).map_err(|source| ResolveError::ReadSource {
            path: skill_path.clone(),
            source,
        })?;
    if canonical_skill.parent() != Some(canonical_source.as_path()) {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: skill_path,
            reason: "bootstrap SKILL.md must be contained by its source directory",
        });
    }

    let raw = match std::fs::read_to_string(&skill_path) {
        Ok(raw) => raw,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(ResolveError::CatalogBootstrapMissing { path: skill_path });
        }
        Err(source) => {
            return Err(ResolveError::ReadSource {
                path: skill_path,
                source,
            });
        }
    };
    let parsed = crate::skills::frontmatter::parse_source(&raw);
    if parsed.name != Some(CATALOG_BOOTSTRAP_ID) {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: skill_path,
            reason: "frontmatter name must match the canonical bundled skill id",
        });
    }
    let Some(description) = parsed
        .description
        .as_deref()
        .filter(|description| !description.trim().is_empty())
    else {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: skill_path,
            reason: "frontmatter description must be present and non-empty",
        });
    };
    if parsed.role != Some(SkillRole::Both) {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: skill_path,
            reason: "frontmatter role must be both",
        });
    }
    if parsed.body.trim().is_empty() {
        return Err(ResolveError::CatalogBootstrapIntegrity {
            path: skill_path,
            reason: "instruction body must be non-empty",
        });
    }

    let candidate = SkillSourceCandidate {
        payload: SkillPayload {
            id: CATALOG_BOOTSTRAP_ID.to_owned(),
            description: description.to_owned(),
            body: parsed.body.to_owned(),
            source: SkillSource::Bundled,
            role: SkillRole::Both,
        },
        source_dir,
    };
    Ok(vec![resolve_candidate(
        candidate,
        ResolvedSourceKind::Bundled,
    )?])
}

fn resolve_candidate(
    candidate: SkillSourceCandidate,
    kind: ResolvedSourceKind,
) -> Result<ResolvedSkill, ResolveError> {
    crate::skills::validate_id(&candidate.payload.id)?;
    let content_sha256 = hash_source_dir(&candidate.source_dir)?;
    Ok(ResolvedSkill {
        payload: candidate.payload,
        source: ResolvedSource {
            kind,
            content_sha256,
            source_dir: candidate.source_dir,
        },
    })
}

fn hash_source_dir(path: &Path) -> Result<String, ResolveError> {
    crate::explore::content_hash(path).map_err(|error| match error.downcast::<std::io::Error>() {
        Ok(source) => ResolveError::ReadSource {
            path: path.to_path_buf(),
            source,
        },
        Err(error) => ResolveError::Manifest(error),
    })
}

#[cfg(test)]
mod tests {
    use super::{resolve_effective_skills, ResolveError, ResolvedSourceKind};
    use crate::explore::catalog::ItemKind;
    use crate::explore::pool::{pool_dir, GateRecord, Manifest, ManifestItem};
    use crate::skills::adapters::Adapter;
    use crate::skills::projection::{RuntimeRole, SelectionPolicy};
    use crate::skills::SkillSource;
    use spur_acp::types::AgentKind;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    struct ResolverHarness {
        repo: tempfile::TempDir,
        assets: tempfile::TempDir,
    }

    impl ResolverHarness {
        fn new() -> Self {
            let repo = tempfile::tempdir().unwrap();
            let assets = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(repo.path().join(".spur")).unwrap();
            std::fs::write(
                repo.path().join(".spur/config.toml"),
                format!("[skills]\nbundled_dir = \"{}\"\n", toml_path(assets.path())),
            )
            .unwrap();
            Self { repo, assets }
        }

        fn write_bundled(&self, id: &str, role: &str, body: &str) {
            write_skill(self.assets.path(), id, role, body);
        }

        fn write_override(&self, id: &str, role: &str, body: &str) {
            write_skill(&self.repo.path().join(".spur/skills"), id, role, body);
        }

        fn write_pool(&self, id: &str, verdict: &str, body: &str) -> PathBuf {
            let source = "acme/skills";
            let pinned_commit = "abcdef1234567890abcdef1234567890abcdef12";
            let source_dir = pool_dir(self.repo.path(), source, id, pinned_commit);
            write_skill_source(&source_dir, id, "both", body);
            let content_sha256 = crate::explore::content_hash(&source_dir).unwrap();
            let item = ManifestItem {
                name: id.to_string(),
                kind: ItemKind::Skill,
                source: source.to_string(),
                rel_path: format!("skills/{id}"),
                pinned_commit: pinned_commit.to_string(),
                content_sha256,
                license: None,
                gate: GateRecord {
                    verdict: verdict.to_string(),
                    justification: None,
                    decided_at_epoch: None,
                },
            };
            let mut manifest = Manifest::load(self.repo.path()).unwrap();
            manifest.items.retain(|existing| existing.name != id);
            manifest.items.push(item);
            manifest.save(self.repo.path()).unwrap();
            source_dir
        }

        fn resolve_with_policy(
            &self,
            adapter: Adapter,
            policy: SelectionPolicy,
        ) -> Result<Vec<super::ResolvedSkill>, ResolveError> {
            resolve_effective_skills(self.repo.path(), adapter, RuntimeRole::Worker, policy)
        }

        fn resolve(&self, adapter: Adapter) -> Result<Vec<super::ResolvedSkill>, ResolveError> {
            self.resolve_with_policy(adapter, SelectionPolicy::AllActive)
        }
    }

    #[test]
    fn all_active_resolves_precedence_and_keeps_bundled_brain_skills() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("repo-wins", "both", "bundled repo body\n");
        fixture.write_bundled("pool-wins", "both", "bundled pool body\n");
        fixture.write_bundled("bundled-only", "both", "bundled only body\n");
        fixture.write_bundled("brain-builtin", "brain", "brain body\n");
        fixture.write_pool("repo-wins", "replaced-bundled", "pool repo body\n");
        fixture.write_pool("pool-wins", "replaced-bundled", "pool body\n");
        fixture.write_pool("pool-only", "clean", "pool only body\n");
        fixture.write_override("repo-wins", "worker", "repository body\n");

        let resolved = fixture.resolve(Adapter::Codex).unwrap();
        let ids = resolved
            .iter()
            .map(|skill| skill.payload.id.as_str())
            .collect::<Vec<_>>();
        let by_id = resolved
            .iter()
            .map(|skill| (skill.payload.id.as_str(), skill.source.kind))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            ids,
            vec![
                "brain-builtin",
                "bundled-only",
                "pool-only",
                "pool-wins",
                "repo-wins",
            ]
        );
        assert_eq!(by_id["repo-wins"], ResolvedSourceKind::RepositoryOverride);
        assert_eq!(by_id["pool-wins"], ResolvedSourceKind::Pool);
        assert_eq!(by_id["pool-only"], ResolvedSourceKind::Pool);
        assert_eq!(by_id["bundled-only"], ResolvedSourceKind::Bundled);
        assert_eq!(by_id["brain-builtin"], ResolvedSourceKind::Bundled);
        assert!(matches!(
            resolved
                .iter()
                .find(|skill| skill.payload.id == "pool-only")
                .unwrap()
                .payload
                .source,
            SkillSource::Pool
        ));
    }

    #[test]
    fn catalog_only_resolves_exactly_the_bundled_bootstrap() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("skills-catalog", "both", "bootstrap body\n");
        fixture.write_bundled("other-bundled", "both", "other body\n");
        fixture.write_pool("pool-only", "clean", "pool body\n");
        fixture.write_override("override-only", "worker", "override body\n");

        let resolved = fixture
            .resolve_with_policy(Adapter::Codex, SelectionPolicy::CatalogOnly)
            .unwrap();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].payload.id, "skills-catalog");
        assert_eq!(resolved[0].payload.body, "bootstrap body\n");
        assert_eq!(resolved[0].source.kind, ResolvedSourceKind::Bundled);
    }

    #[test]
    fn catalog_only_ignores_pool_and_repository_bootstrap_shadows() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("skills-catalog", "both", "bundled body\n");
        fixture.write_pool("skills-catalog", "replaced-bundled", "pool shadow body\n");
        fixture.write_override("skills-catalog", "worker", "override shadow body\n");

        let resolved = fixture
            .resolve_with_policy(Adapter::Codex, SelectionPolicy::CatalogOnly)
            .unwrap();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].payload.id, "skills-catalog");
        assert_eq!(resolved[0].payload.body, "bundled body\n");
        assert_eq!(resolved[0].source.kind, ResolvedSourceKind::Bundled);
    }

    #[test]
    fn catalog_only_fails_closed_when_bootstrap_is_missing() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("other-bundled", "both", "other body\n");

        let error = fixture
            .resolve_with_policy(Adapter::Codex, SelectionPolicy::CatalogOnly)
            .unwrap_err();

        assert!(matches!(
            error,
            ResolveError::CatalogBootstrapMissing { .. }
        ));
    }

    #[test]
    fn catalog_only_fails_closed_when_bootstrap_identity_is_invalid() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        write_skill_source(
            &fixture.assets.path().join("skills-catalog"),
            "different-name",
            "both",
            "bootstrap body\n",
        );

        let error = fixture
            .resolve_with_policy(Adapter::Codex, SelectionPolicy::CatalogOnly)
            .unwrap_err();

        assert!(matches!(
            error,
            ResolveError::CatalogBootstrapIntegrity { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn catalog_only_rejects_symlinked_bootstrap_directory_during_resolution() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_override("skills-catalog", "both", "repository shadow body\n");
        std::os::unix::fs::symlink(
            fixture.repo.path().join(".spur/skills/skills-catalog"),
            fixture.assets.path().join("skills-catalog"),
        )
        .unwrap();

        let error = fixture
            .resolve_with_policy(Adapter::Codex, SelectionPolicy::CatalogOnly)
            .unwrap_err();

        assert!(matches!(
            error,
            ResolveError::CatalogBootstrapIntegrity { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn catalog_only_rejects_symlinked_bootstrap_file_during_resolution() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        let shadow_dir = fixture.repo.path().join(".spur/skills/catalog-shadow");
        write_skill_source(
            &shadow_dir,
            "skills-catalog",
            "both",
            "repository shadow body\n",
        );
        let bootstrap_dir = fixture.assets.path().join("skills-catalog");
        std::fs::create_dir_all(&bootstrap_dir).unwrap();
        std::os::unix::fs::symlink(shadow_dir.join("SKILL.md"), bootstrap_dir.join("SKILL.md"))
            .unwrap();

        let error = fixture
            .resolve_with_policy(Adapter::Codex, SelectionPolicy::CatalogOnly)
            .unwrap_err();

        assert!(matches!(
            error,
            ResolveError::CatalogBootstrapIntegrity { .. }
        ));
    }

    #[test]
    fn ineligible_brain_override_falls_back_to_bundled_for_external_adapter() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("shadowed", "both", "bundled body\n");
        fixture.write_override("shadowed", "brain", "override body\n");

        let codex = fixture.resolve(Adapter::Codex).unwrap();
        let hermetic = fixture.resolve(Adapter::SpurHermetic).unwrap();

        assert_eq!(codex[0].source.kind, ResolvedSourceKind::Bundled);
        assert_eq!(codex[0].payload.body, "bundled body\n");
        assert_eq!(
            hermetic[0].source.kind,
            ResolvedSourceKind::RepositoryOverride
        );
        assert_eq!(hermetic[0].payload.body, "override body\n");
    }

    #[test]
    fn rejected_pool_verdict_is_a_typed_error() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("bundled-only", "both", "bundled body\n");
        fixture.write_pool("rejected", "blocked", "blocked body\n");

        let error = fixture.resolve(Adapter::Codex).unwrap_err();

        assert!(matches!(error, ResolveError::Manifest(_)));
        assert!(error.to_string().contains("rejected"));
        assert!(error.to_string().contains("blocked"));
    }

    #[test]
    fn pool_digest_mismatch_is_a_typed_error() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("bundled-only", "both", "bundled body\n");
        let source_dir = fixture.write_pool("tampered", "clean", "original body\n");
        write_skill_source(&source_dir, "tampered", "both", "changed body\n");

        let error = fixture.resolve(Adapter::Codex).unwrap_err();

        assert!(matches!(
            error,
            ResolveError::PoolDigestMismatch { ref id, .. } if id == "tampered"
        ));
    }

    #[test]
    fn bundled_collision_requires_replacement_authorization() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        fixture.write_bundled("collision", "both", "bundled body\n");
        fixture.write_pool("collision", "clean", "pool body\n");

        let error = fixture.resolve(Adapter::Codex).unwrap_err();

        assert!(matches!(
            error,
            ResolveError::PoolReplacementNotAuthorized { ref id } if id == "collision"
        ));
    }

    #[test]
    fn pool_source_path_traversal_is_rejected() {
        let _global_root = crate::explore::store::force_global_root_for_tests(None);
        let fixture = ResolverHarness::new();
        let id = "escaped";
        let pinned_commit = "abcdef1234567890abcdef1234567890abcdef12";
        let explore_root = fixture.repo.path().join(".spur/explore");
        std::fs::create_dir_all(explore_root.join("pool")).unwrap();
        let escaped_dir = explore_root.join(format!("{id}@{}", &pinned_commit[..7]));
        write_skill_source(&escaped_dir, id, "both", "escaped body\n");
        let content_sha256 = crate::explore::content_hash(&escaped_dir).unwrap();
        Manifest {
            sources: Vec::new(),
            items: vec![ManifestItem {
                name: id.to_string(),
                kind: ItemKind::Skill,
                source: "..".to_string(),
                rel_path: format!("skills/{id}"),
                pinned_commit: pinned_commit.to_string(),
                content_sha256,
                license: None,
                gate: GateRecord {
                    verdict: "clean".to_string(),
                    justification: None,
                    decided_at_epoch: None,
                },
            }],
        }
        .save(fixture.repo.path())
        .unwrap();

        let error = fixture.resolve(Adapter::Codex).unwrap_err();

        assert!(matches!(error, ResolveError::Manifest(_)));
        assert!(error.to_string().contains("unsafe pool source path"));
    }

    #[test]
    fn adapter_identity_helpers_cover_supported_agent_kinds() {
        use AgentKind::{
            ClaudeCodeAcp, ClaudeStreamJson, CodexAcp, Gemini, Generic, Grok, Kimi, Kiro, OpenCode,
        };

        assert_eq!(Adapter::SpurHermetic.key(), "spur-hermetic");
        assert_eq!(Adapter::ClaudeCode.key(), "claude-code");
        assert_eq!(Adapter::Codex.key(), "codex");
        assert_eq!(Adapter::Gemini.key(), "gemini");
        assert_eq!(Adapter::Kiro.key(), "kiro");
        assert_eq!(Adapter::OpenCode.key(), "opencode");
        assert_eq!(Adapter::Cursor.key(), "cursor");
        assert_eq!(Adapter::Kimi.key(), "kimi");
        assert_eq!(
            Adapter::for_agent_kind(ClaudeStreamJson),
            Some(Adapter::ClaudeCode)
        );
        assert_eq!(
            Adapter::for_agent_kind(ClaudeCodeAcp),
            Some(Adapter::ClaudeCode)
        );
        assert_eq!(Adapter::for_agent_kind(CodexAcp), Some(Adapter::Codex));
        assert_eq!(Adapter::for_agent_kind(Gemini), Some(Adapter::Gemini));
        assert_eq!(Adapter::for_agent_kind(Kiro), Some(Adapter::Kiro));
        assert_eq!(Adapter::for_agent_kind(OpenCode), Some(Adapter::OpenCode));
        assert_eq!(Adapter::for_agent_kind(Kimi), Some(Adapter::Kimi));
        assert_eq!(Adapter::for_agent_kind(Grok), None);
        assert_eq!(Adapter::for_agent_kind(Generic), None);
        assert!(!Adapter::Cursor.target_is_directory());
        assert!(Adapter::Codex.target_is_directory());
    }

    fn write_skill(root: &Path, id: &str, role: &str, body: &str) {
        write_skill_source(&root.join(id), id, role, body);
    }

    fn write_skill_source(source_dir: &Path, id: &str, role: &str, body: &str) {
        std::fs::create_dir_all(source_dir).unwrap();
        std::fs::write(
            source_dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: {id} description\nrole: {role}\n---\n{body}"),
        )
        .unwrap();
    }

    fn toml_path(path: &Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }
}
