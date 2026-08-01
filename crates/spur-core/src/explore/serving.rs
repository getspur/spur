//! Read-only, repository-scoped serving policy for agent skill discovery.

use crate::explore::catalog::{Catalog, CatalogEntry, ItemKind};
use crate::explore::pool::{Manifest, ManifestItem};
use anyhow::Context as _;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Agent text content size policy, solved by `sol_ece03f4a166e4004`.
pub const MAX_TEXT_CONTENT_BYTES: usize = 262_144;

/// Lede length for PageIndex skill nodes (matches `doc_navigate`).
const LEDE_CHARS: usize = 200;

/// Default and maximum result count from the approved API contract.
pub const DEFAULT_SEARCH_LIMIT: usize = 5;
pub const MAX_SEARCH_LIMIT: usize = 5;

/// Inputs to the agent-serving eligibility policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EligibilityInput {
    pub bundled: bool,
    pub adopted: bool,
    pub gate_approved: bool,
    pub enabled: bool,
    pub compatible: bool,
}

/// Result of applying the agent-serving eligibility policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    Eligible,
    Ineligible,
}

/// Applies the `SKILL-ELIGIBILITY-POLICY` contract.
#[must_use]
pub const fn evaluate_eligibility(input: EligibilityInput) -> Eligibility {
    if input.enabled
        && input.compatible
        && (input.bundled || (input.adopted && input.gate_approved))
    {
        Eligibility::Eligible
    } else {
        Eligibility::Ineligible
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCompatibility {
    Compatible,
    MissingContent,
    RequiresScripts,
    BinaryResources,
    NonUtf8Text,
    SymlinkResource,
    UnsafeResourcePath,
    ContentTooLarge,
    IntegrityMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServingErrorKind {
    InvalidQuery,
    SkillNotFound,
    SkillNotEligible,
    StaleSkillRef,
    ResourceNotFound,
    ResourceDenied,
    ContentTooLarge,
    IntegrityMismatch,
}

impl ServingErrorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidQuery => "invalid_query",
            Self::SkillNotFound => "skill_not_found",
            Self::SkillNotEligible => "skill_not_eligible",
            Self::StaleSkillRef => "stale_skill_ref",
            Self::ResourceNotFound => "resource_not_found",
            Self::ResourceDenied => "resource_denied",
            Self::ContentTooLarge => "content_too_large",
            Self::IntegrityMismatch => "integrity_mismatch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}: {message}", .kind.as_str())]
pub struct ServingError {
    kind: ServingErrorKind,
    message: String,
}

impl ServingError {
    fn new(kind: ServingErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ServingErrorKind {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    pub skill_id: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub pinned_commit: Option<String>,
    pub content_sha256: String,
    pub resource_manifest_sha256: String,
    pub compatibility: ContextCompatibility,
    pub availability: Eligibility,
    pub rank: usize,
    pub match_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchResponse {
    pub catalog_revision: String,
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReadResponse {
    pub skill_id: String,
    pub name: String,
    pub source: String,
    pub catalog_revision: String,
    pub content_sha256: String,
    pub resource: String,
    pub media_type: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingDecision {
    pub eligibility: Eligibility,
    pub compatibility: ContextCompatibility,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct VersionIdentity {
    source: String,
    rel_path: String,
    pinned_commit: String,
    content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillReference {
    lineage_sha256: String,
    version_sha256: String,
}

impl SkillReference {
    fn encode(&self) -> String {
        format!(
            "skillref.v1.{}.{}",
            self.lineage_sha256, self.version_sha256
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextResource {
    path: String,
    media_type: String,
    size: usize,
    sha256: String,
}

/// Kind of a navigable PageIndex node built for an eligible skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillNavNodeKind {
    Frontmatter,
    Document,
    Section,
    Resource,
}

/// In-memory PageIndex node for skill navigation (FTS + tree hop).
///
/// Built only for eligible skills from frontmatter metadata, SKILL.md
/// headings/section bodies (post-frontmatter strip), and approved text resources.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillNavNode {
    skill_id: String,
    node_id: String,
    kind: SkillNavNodeKind,
    path: String,
    heading: Option<String>,
    heading_level: Option<u8>,
    parent_node_id: Option<String>,
    child_count: usize,
    lede: String,
    tokens: Vec<String>,
}

#[derive(Debug, Clone)]
struct ServingState {
    identity: VersionIdentity,
    name: String,
    description: String,
    source: String,
    skill_id: String,
    lineage_sha256: String,
    source_dir: PathBuf,
    main: Option<TextResource>,
    resources: BTreeMap<String, TextResource>,
    resource_manifest_sha256: String,
    /// PageIndex nodes for eligible skills; empty when ineligible.
    nodes: Vec<SkillNavNode>,
    input: EligibilityInput,
    eligibility: Eligibility,
    compatibility: ContextCompatibility,
    gate_verdict: Option<String>,
    precedence: u8,
}

#[derive(Debug, Clone)]
pub struct ServingCatalog {
    repo_root: PathBuf,
    bundled_root: PathBuf,
    global_root: Option<PathBuf>,
    rediscover_roots: bool,
    revision: String,
    states: Vec<ServingState>,
    eligible_indices: Vec<usize>,
}

impl ServingCatalog {
    pub fn load(repo_root: &Path) -> anyhow::Result<Self> {
        let bundled = crate::skills::SkillCatalog::discover(repo_root)?;
        let bundled_raw = bundled.list_raw(repo_root)?;
        let global_root = crate::explore::store::global_root();
        let merged_catalog = Catalog::load_merged(repo_root)?;
        let layered_manifest = Manifest::load_layered(repo_root)?;
        Self::build(
            repo_root,
            bundled.bundled_root(),
            global_root.as_deref(),
            bundled_raw,
            merged_catalog,
            layered_manifest,
            true,
        )
    }

    fn load_from_roots(
        repo_root: &Path,
        bundled_root: &Path,
        global_root: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let bundled_raw = read_bundled_raw(bundled_root)?;
        let merged_catalog = Catalog::load_merged_from_roots(repo_root, global_root)?;
        let layered_manifest = Manifest::load_layered_from_roots(repo_root, global_root)?;
        Self::build(
            repo_root,
            bundled_root,
            global_root,
            bundled_raw,
            merged_catalog,
            layered_manifest,
            false,
        )
    }

    fn build(
        repo_root: &Path,
        bundled_root: &Path,
        global_root: Option<&Path>,
        bundled_raw: Vec<(String, String)>,
        merged_catalog: Catalog,
        layered_manifest: Manifest,
        rediscover_roots: bool,
    ) -> anyhow::Result<Self> {
        let local_catalog = Catalog::load(repo_root)?;
        let local_manifest = Manifest::load(repo_root)?;
        let (global_catalog, global_manifest) = match global_root.filter(|root| root.exists()) {
            Some(root) => (
                Catalog::load_from_store(root)?,
                Manifest::load_from_store(root)?,
            ),
            None => (Catalog::default(), Manifest::default()),
        };
        let mut by_identity = BTreeMap::<String, ServingState>::new();
        for (id, raw) in bundled_raw {
            let source_dir = bundled_root.join(&id);
            if !source_dir.is_dir() {
                continue;
            }
            let parsed = crate::skills::frontmatter::parse_source(&raw);
            let name = parsed.name.unwrap_or(&id).trim().to_string();
            let description = parsed
                .description
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string();
            let state = build_state(
                name,
                description,
                "bundled".to_string(),
                id,
                String::new(),
                None,
                source_dir,
                None,
                true,
                2,
            );
            insert_preferred_state(&mut by_identity, state);
        }

        if let Some(global_root) = global_root.filter(|root| root.exists()) {
            for entry in global_catalog
                .entries
                .iter()
                .filter(|entry| entry.kind == ItemKind::Skill)
            {
                let manifest_item = matching_manifest_item(entry, &global_manifest);
                let (source_dir, compatibility_override) = confined_pool_dir(global_root, entry);
                let state =
                    external_state(entry, manifest_item, source_dir, compatibility_override, 0);
                insert_preferred_state(&mut by_identity, state);
            }
        }

        for entry in merged_catalog
            .entries
            .iter()
            .filter(|entry| entry.kind == ItemKind::Skill)
        {
            let (manifest_item, store_root) = if catalog_contains_entry(&local_catalog, entry) {
                (
                    matching_manifest_item(entry, &local_manifest),
                    crate::explore::store::local_root(repo_root),
                )
            } else if catalog_contains_entry(&global_catalog, entry) {
                (
                    matching_manifest_item(entry, &global_manifest),
                    global_root
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| crate::explore::store::local_root(repo_root)),
                )
            } else {
                (
                    matching_manifest_item(entry, &layered_manifest),
                    crate::explore::store::local_root(repo_root),
                )
            };
            let (source_dir, compatibility_override) = confined_pool_dir(&store_root, entry);
            let state = external_state(entry, manifest_item, source_dir, compatibility_override, 1);
            insert_preferred_state(&mut by_identity, state);
        }

        let mut states: Vec<_> = by_identity.into_values().collect();
        for state in &mut states {
            if state.eligibility == Eligibility::Eligible {
                state.nodes = build_skill_pageindex(state);
            }
        }
        let eligible_indices = choose_eligible_indices(&states);
        let revision = catalog_revision(&states, &eligible_indices)?;
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            bundled_root: bundled_root.to_path_buf(),
            global_root: global_root.map(Path::to_path_buf),
            rediscover_roots,
            revision,
            states,
            eligible_indices,
        })
    }

    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn search(
        &self,
        query: &str,
        limit: Option<usize>,
        source: Option<&str>,
    ) -> Result<SearchResponse, ServingError> {
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        let query_tokens = tokenize(query);
        if query.trim().is_empty()
            || query_tokens.is_empty()
            || !(1..=MAX_SEARCH_LIMIT).contains(&limit)
        {
            return Err(ServingError::new(
                ServingErrorKind::InvalidQuery,
                "query must be non-empty and limit must be between 1 and 5",
            ));
        }
        let candidates: Vec<_> = self
            .eligible_indices
            .iter()
            .copied()
            .filter(|index| source.is_none_or(|source| self.states[*index].source == source))
            .collect();
        let scores = bm25_scores(&self.states, &candidates, &query_tokens);
        let query_set: BTreeSet<_> = query_tokens.iter().cloned().collect();
        let normalized_query = query_tokens.join(" ");
        let mut ranked = candidates
            .into_iter()
            .zip(scores)
            .filter_map(|(index, score)| {
                let state = &self.states[index];
                let name_tokens = tokenize(&state.name);
                let document_tokens = document_tokens(state);
                let name_token_matches = query_set
                    .iter()
                    .filter(|token| name_tokens.contains(*token))
                    .count();
                let matched: Vec<_> = query_set
                    .iter()
                    .filter(|token| document_tokens.contains(*token))
                    .cloned()
                    .collect();
                (!matched.is_empty()).then_some((
                    index,
                    name_tokens.join(" ") == normalized_query,
                    name_token_matches,
                    matched.len(),
                    score,
                    matched,
                ))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| right.3.cmp(&left.3))
                .then_with(|| right.4.total_cmp(&left.4))
                .then_with(|| {
                    identity_sort_key(&self.states[left.0].identity)
                        .cmp(&identity_sort_key(&self.states[right.0].identity))
                })
        });
        let results = ranked
            .into_iter()
            .take(limit)
            .enumerate()
            .map(|(offset, (index, exact_name, _, _, _, matched))| {
                let state = &self.states[index];
                SearchResult {
                    skill_id: state.skill_id.clone(),
                    name: state.name.clone(),
                    description: state.description.clone(),
                    source: state.source.clone(),
                    pinned_commit: (!state.identity.pinned_commit.is_empty())
                        .then(|| state.identity.pinned_commit.clone()),
                    content_sha256: state.identity.content_sha256.clone(),
                    resource_manifest_sha256: state.resource_manifest_sha256.clone(),
                    compatibility: state.compatibility,
                    availability: state.eligibility,
                    rank: offset + 1,
                    match_reason: if exact_name {
                        "exact name match".to_string()
                    } else {
                        format!("matched tokens: {}", matched.join(", "))
                    },
                }
            })
            .collect();
        Ok(SearchResponse {
            catalog_revision: self.revision.clone(),
            results,
        })
    }

    pub fn read(
        &self,
        skill_id: &str,
        resource: Option<&str>,
    ) -> Result<ReadResponse, ServingError> {
        let reference = decode_skill_ref(skill_id).ok_or_else(|| {
            ServingError::new(
                ServingErrorKind::SkillNotFound,
                "unknown opaque skill reference",
            )
        })?;
        let normalized_resource = resource.map(validate_resource_path).transpose()?;
        let existed_in_snapshot = self.states.iter().any(|state| state.skill_id == skill_id);
        let current = self.reload().map_err(|error| {
            ServingError::new(
                ServingErrorKind::SkillNotEligible,
                format!("current catalog state is unavailable: {error:#}"),
            )
        })?;
        let Some((current_index, state)) = current
            .states
            .iter()
            .enumerate()
            .find(|(_, state)| state.skill_id == skill_id)
        else {
            let kind = if current
                .states
                .iter()
                .any(|state| state.lineage_sha256 == reference.lineage_sha256)
            {
                ServingErrorKind::StaleSkillRef
            } else if existed_in_snapshot {
                ServingErrorKind::SkillNotEligible
            } else {
                ServingErrorKind::SkillNotFound
            };
            return Err(ServingError::new(kind, "skill reference is not current"));
        };
        match state.compatibility {
            ContextCompatibility::ContentTooLarge => {
                return Err(ServingError::new(
                    ServingErrorKind::ContentTooLarge,
                    "skill content exceeds the agent text content size policy",
                ));
            }
            ContextCompatibility::IntegrityMismatch => {
                return Err(ServingError::new(
                    ServingErrorKind::IntegrityMismatch,
                    "pinned skill content hash does not match",
                ));
            }
            _ => {}
        }
        if state.eligibility != Eligibility::Eligible
            || !current.eligible_indices.contains(&current_index)
        {
            return Err(ServingError::new(
                ServingErrorKind::SkillNotEligible,
                "skill is not currently eligible",
            ));
        }

        let (resource_name, descriptor) = match normalized_resource.as_deref() {
            None | Some("SKILL.md") => ("SKILL.md", state.main.as_ref()),
            Some(path) => (path, state.resources.get(path)),
        };
        let descriptor = descriptor.ok_or_else(|| {
            ServingError::new(
                ServingErrorKind::ResourceNotFound,
                "resource is absent from the approved text inventory",
            )
        })?;
        let content = read_verified_text(&state.source_dir, descriptor)?;
        let actual = crate::explore::content_hash(&state.source_dir).map_err(|error| {
            ServingError::new(
                ServingErrorKind::IntegrityMismatch,
                format!("recheck skill content hash: {error:#}"),
            )
        })?;
        if actual != state.identity.content_sha256 {
            return Err(ServingError::new(
                ServingErrorKind::IntegrityMismatch,
                "skill changed while it was being read",
            ));
        }
        Ok(ReadResponse {
            skill_id: state.skill_id.clone(),
            name: state.name.clone(),
            source: state.source.clone(),
            catalog_revision: current.revision,
            content_sha256: state.identity.content_sha256.clone(),
            resource: resource_name.to_string(),
            media_type: descriptor.media_type.clone(),
            content,
        })
    }

    #[must_use]
    pub fn decision(&self, name: &str, source: &str) -> Option<ServingDecision> {
        self.states
            .iter()
            .filter(|state| state.name == name && state.source == source)
            .max_by_key(|state| state.precedence)
            .map(|state| ServingDecision {
                eligibility: state.eligibility,
                compatibility: state.compatibility,
            })
    }

    fn reload(&self) -> anyhow::Result<Self> {
        if self.rediscover_roots {
            Self::load(&self.repo_root)
        } else {
            Self::load_from_roots(
                &self.repo_root,
                &self.bundled_root,
                self.global_root.as_deref(),
            )
        }
    }
}

fn read_bundled_raw(root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let mut entries = std::fs::read_dir(root)
        .with_context(|| format!("read bundled skill root {}", root.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut raw = Vec::new();
    for entry in entries {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let skill_path = entry.path().join("SKILL.md");
        if !skill_path.is_file() {
            continue;
        }
        raw.push((
            entry.file_name().to_string_lossy().into_owned(),
            std::fs::read_to_string(&skill_path)
                .with_context(|| format!("read {}", skill_path.display()))?,
        ));
    }
    Ok(raw)
}

fn external_state(
    entry: &CatalogEntry,
    manifest_item: Option<&ManifestItem>,
    source_dir: PathBuf,
    compatibility_override: Option<ContextCompatibility>,
    precedence: u8,
) -> ServingState {
    build_state(
        entry.name.clone(),
        entry.description.clone(),
        entry.source.clone(),
        entry.rel_path.clone(),
        entry.pinned_commit.clone(),
        Some(entry.content_sha256.clone()),
        source_dir,
        compatibility_override,
        false,
        precedence,
    )
    .with_manifest_item(manifest_item)
}

impl ServingState {
    fn with_manifest_item(mut self, item: Option<&ManifestItem>) -> Self {
        self.input.adopted = item.is_some();
        self.input.gate_approved = item.is_some_and(|item| gate_is_approved(&item.gate.verdict));
        self.gate_verdict = item.map(|item| item.gate.verdict.clone());
        self.input.enabled &= item.is_none_or(|item| item.gate.verdict != "disabled");
        self.eligibility = evaluate_eligibility(self.input);
        if self.gate_verdict.as_deref() == Some("replaced-bundled") {
            self.precedence = 3;
        }
        self
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the arguments are the complete immutable catalog identity and policy inputs"
)]
fn build_state(
    name: String,
    description: String,
    source: String,
    rel_path: String,
    pinned_commit: String,
    expected_sha256: Option<String>,
    source_dir: PathBuf,
    compatibility_override: Option<ContextCompatibility>,
    bundled: bool,
    precedence: u8,
) -> ServingState {
    let identity_hash = expected_sha256
        .clone()
        .or_else(|| crate::explore::content_hash(&source_dir).ok())
        .unwrap_or_default();
    let identity = VersionIdentity {
        source: source.clone(),
        rel_path,
        pinned_commit,
        content_sha256: identity_hash,
    };
    let scan = compatibility_override.map_or_else(
        || scan_skill_directory(&source_dir, expected_sha256.as_deref()),
        empty_directory_scan,
    );
    let compatibility = scan.compatibility;
    let input = EligibilityInput {
        bundled,
        adopted: false,
        gate_approved: false,
        enabled: source_dir.is_dir()
            && !matches!(
                compatibility,
                ContextCompatibility::MissingContent | ContextCompatibility::IntegrityMismatch
            ),
        compatible: compatibility == ContextCompatibility::Compatible,
    };
    ServingState {
        skill_id: encode_skill_ref(&identity),
        lineage_sha256: lineage_sha256(&identity),
        identity,
        name,
        description,
        source,
        source_dir,
        main: scan.main,
        resources: scan.resources,
        resource_manifest_sha256: scan.resource_manifest_sha256,
        nodes: Vec::new(),
        input,
        eligibility: evaluate_eligibility(input),
        compatibility,
        gate_verdict: None,
        precedence,
    }
}

fn matching_manifest_item<'a>(
    entry: &CatalogEntry,
    manifest: &'a Manifest,
) -> Option<&'a ManifestItem> {
    manifest.items.iter().find(|item| {
        item.kind == entry.kind
            && item.name == entry.name
            && item.source == entry.source
            && item.rel_path == entry.rel_path
            && item.pinned_commit == entry.pinned_commit
            && item.content_sha256 == entry.content_sha256
    })
}

fn catalog_contains_entry(catalog: &Catalog, entry: &CatalogEntry) -> bool {
    catalog.entries.iter().any(|candidate| {
        candidate.kind == entry.kind
            && candidate.name == entry.name
            && candidate.source == entry.source
            && candidate.rel_path == entry.rel_path
            && candidate.pinned_commit == entry.pinned_commit
            && candidate.content_sha256 == entry.content_sha256
    })
}

fn confined_pool_dir(
    store_root: &Path,
    entry: &CatalogEntry,
) -> (PathBuf, Option<ContextCompatibility>) {
    let pool_root = store_root.join("pool");
    let owner = entry.source.split('/').next().unwrap_or(&entry.source);
    let short_commit = entry.pinned_commit.get(..7).unwrap_or(&entry.pinned_commit);
    if !safe_pool_path_part(owner)
        || !safe_pool_path_part(&entry.name)
        || !safe_pool_path_part(short_commit)
    {
        return (pool_root, Some(ContextCompatibility::UnsafeResourcePath));
    }

    let candidate = crate::explore::pool::pool_dir_in_store(
        store_root,
        &entry.source,
        &entry.name,
        &entry.pinned_commit,
    );
    if !candidate.starts_with(&pool_root) {
        return (pool_root, Some(ContextCompatibility::UnsafeResourcePath));
    }
    let Ok(metadata) = std::fs::symlink_metadata(&candidate) else {
        return (candidate, None);
    };
    if metadata.file_type().is_symlink() {
        return (candidate, None);
    }
    let confined = std::fs::canonicalize(&pool_root)
        .and_then(|root| std::fs::canonicalize(&candidate).map(|candidate| (root, candidate)))
        .is_ok_and(|(root, candidate)| candidate.starts_with(root));
    if confined {
        (candidate, None)
    } else {
        (pool_root, Some(ContextCompatibility::UnsafeResourcePath))
    }
}

fn safe_pool_path_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part.contains('/')
        && !part.contains('\\')
        && !part.contains('\0')
}

fn insert_preferred_state(states: &mut BTreeMap<String, ServingState>, state: ServingState) {
    let key = identity_sort_key(&state.identity);
    let keep_existing = states.get(&key).is_some_and(|existing| {
        (existing.eligibility == Eligibility::Eligible
            && state.eligibility == Eligibility::Ineligible)
            || (existing.eligibility == state.eligibility
                && existing.precedence >= state.precedence)
    });
    if !keep_existing {
        states.insert(key, state);
    }
}

fn gate_is_approved(verdict: &str) -> bool {
    matches!(verdict, "clean" | "overridden" | "replaced-bundled")
}

fn choose_eligible_indices(states: &[ServingState]) -> Vec<usize> {
    let mut chosen = BTreeMap::<&str, usize>::new();
    for (index, state) in states.iter().enumerate() {
        if state.eligibility != Eligibility::Eligible {
            continue;
        }
        match chosen.get(state.name.as_str()).copied() {
            Some(existing)
                if states[existing].precedence > state.precedence
                    || (states[existing].precedence == state.precedence
                        && identity_sort_key(&states[existing].identity)
                            <= identity_sort_key(&state.identity)) => {}
            _ => {
                chosen.insert(&state.name, index);
            }
        }
    }
    chosen.into_values().collect()
}

fn catalog_revision(states: &[ServingState], indices: &[usize]) -> anyhow::Result<String> {
    let policy: Vec<_> = indices
        .iter()
        .map(|index| {
            let state = &states[*index];
            (
                &state.identity,
                &state.name,
                &state.description,
                state.compatibility,
                state.input,
                &state.resource_manifest_sha256,
            )
        })
        .collect();
    let bytes = serde_json::to_vec(&policy).context("serialize serving catalog revision")?;
    Ok(format!("sha256:{}", super::sha256_hex(&bytes)))
}

#[derive(Debug)]
struct DirectoryScan {
    main: Option<TextResource>,
    resources: BTreeMap<String, TextResource>,
    resource_manifest_sha256: String,
    compatibility: ContextCompatibility,
}

fn empty_directory_scan(compatibility: ContextCompatibility) -> DirectoryScan {
    DirectoryScan {
        main: None,
        resources: BTreeMap::new(),
        resource_manifest_sha256: super::sha256_hex(&[]),
        compatibility,
    }
}

fn scan_skill_directory(root: &Path, expected_sha256: Option<&str>) -> DirectoryScan {
    let Ok(metadata) = std::fs::symlink_metadata(root) else {
        return empty_directory_scan(ContextCompatibility::MissingContent);
    };
    if metadata.file_type().is_symlink() {
        return empty_directory_scan(ContextCompatibility::SymlinkResource);
    }
    if !metadata.is_dir() {
        return empty_directory_scan(ContextCompatibility::MissingContent);
    }
    let Ok(actual_sha256) = crate::explore::content_hash(root) else {
        return empty_directory_scan(ContextCompatibility::MissingContent);
    };
    if expected_sha256.is_some_and(|expected| expected != actual_sha256) {
        return empty_directory_scan(ContextCompatibility::IntegrityMismatch);
    }
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return empty_directory_scan(ContextCompatibility::MissingContent);
    };
    let mut main = None;
    let mut resources = BTreeMap::new();
    if let Err(compatibility) =
        collect_text_inventory(root, root, &canonical_root, &mut main, &mut resources)
    {
        return DirectoryScan {
            main,
            resources,
            resource_manifest_sha256: super::sha256_hex(&[]),
            compatibility,
        };
    }
    if main.is_none() {
        return empty_directory_scan(ContextCompatibility::MissingContent);
    }
    let mut inventory: Vec<_> = main.iter().chain(resources.values()).collect();
    inventory.sort_by(|left, right| left.path.cmp(&right.path));
    let mut manifest = Vec::new();
    for resource in inventory {
        manifest.extend_from_slice(resource.path.as_bytes());
        manifest.push(0);
        manifest.extend_from_slice(resource.media_type.as_bytes());
        manifest.push(0);
        manifest.extend_from_slice(resource.size.to_string().as_bytes());
        manifest.push(0);
        manifest.extend_from_slice(resource.sha256.as_bytes());
        manifest.push(b'\n');
    }
    DirectoryScan {
        main,
        resources,
        resource_manifest_sha256: super::sha256_hex(&manifest),
        compatibility: ContextCompatibility::Compatible,
    }
}

fn collect_text_inventory(
    root: &Path,
    dir: &Path,
    canonical_root: &Path,
    main: &mut Option<TextResource>,
    resources: &mut BTreeMap<String, TextResource>,
) -> Result<(), ContextCompatibility> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|_| ContextCompatibility::MissingContent)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ContextCompatibility::MissingContent)?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| ContextCompatibility::MissingContent)?;
        if metadata.file_type().is_symlink() {
            return Err(ContextCompatibility::SymlinkResource);
        }
        if metadata.is_dir() {
            collect_text_inventory(root, &path, canonical_root, main, resources)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(ContextCompatibility::BinaryResources);
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ContextCompatibility::UnsafeResourcePath)?;
        if is_script_path(relative) {
            return Err(ContextCompatibility::RequiresScripts);
        }
        let Some(media_type) = text_media_type(relative) else {
            return Err(ContextCompatibility::BinaryResources);
        };
        let size =
            usize::try_from(metadata.len()).map_err(|_| ContextCompatibility::ContentTooLarge)?;
        if size > MAX_TEXT_CONTENT_BYTES {
            return Err(ContextCompatibility::ContentTooLarge);
        }
        let canonical =
            std::fs::canonicalize(&path).map_err(|_| ContextCompatibility::UnsafeResourcePath)?;
        if !canonical.starts_with(canonical_root) {
            return Err(ContextCompatibility::UnsafeResourcePath);
        }
        let bytes = std::fs::read(&path).map_err(|_| ContextCompatibility::MissingContent)?;
        std::str::from_utf8(&bytes).map_err(|_| ContextCompatibility::NonUtf8Text)?;
        let Some(relative) = relative.to_str() else {
            return Err(ContextCompatibility::UnsafeResourcePath);
        };
        let descriptor = TextResource {
            path: relative.replace('\\', "/"),
            media_type: media_type.to_string(),
            size,
            sha256: super::sha256_hex(&bytes),
        };
        if descriptor.path == "SKILL.md" {
            *main = Some(descriptor);
        } else {
            resources.insert(descriptor.path.clone(), descriptor);
        }
    }
    Ok(())
}

fn is_script_path(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, Component::Normal(part) if part.to_string_lossy().eq_ignore_ascii_case("scripts"))
    }) || path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "sh" | "bash" | "zsh" | "fish" | "py" | "js" | "cjs" | "mjs" | "ts"
                    | "ps1" | "bat" | "cmd" | "exe"
            )
        })
}

fn text_media_type(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "markdown" => Some("text/markdown"),
        "txt" => Some("text/plain"),
        "json" => Some("application/json"),
        "toml" => Some("application/toml"),
        "yaml" | "yml" => Some("application/yaml"),
        "csv" => Some("text/csv"),
        "html" => Some("text/html"),
        "css" => Some("text/css"),
        "xml" => Some("application/xml"),
        "rs" => Some("text/x-rust"),
        _ => None,
    }
}

fn validate_resource_path(path: &str) -> Result<String, ServingError> {
    let candidate = Path::new(path);
    let safe = !path.is_empty()
        && !candidate.is_absolute()
        && !path.contains('\\')
        && candidate
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !safe {
        return Err(ServingError::new(
            ServingErrorKind::ResourceDenied,
            "resource path must be a confined normalized relative path",
        ));
    }
    if is_script_path(candidate) || text_media_type(candidate).is_none() {
        return Err(ServingError::new(
            ServingErrorKind::ResourceDenied,
            "resource media type is unsupported for context-only delivery",
        ));
    }
    Ok(path.to_string())
}

fn read_verified_text(root: &Path, descriptor: &TextResource) -> Result<String, ServingError> {
    let path = root.join(&descriptor.path);
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
        ServingError::new(
            ServingErrorKind::ResourceNotFound,
            "resource disappeared from pinned content",
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ServingError::new(
            ServingErrorKind::ResourceDenied,
            "resource is not a confined regular file",
        ));
    }
    let size = usize::try_from(metadata.len()).map_err(|_| {
        ServingError::new(
            ServingErrorKind::ContentTooLarge,
            "resource length exceeds platform limits",
        )
    })?;
    if size > MAX_TEXT_CONTENT_BYTES {
        return Err(ServingError::new(
            ServingErrorKind::ContentTooLarge,
            "resource exceeds the agent text content size policy",
        ));
    }
    if size != descriptor.size {
        return Err(ServingError::new(
            ServingErrorKind::IntegrityMismatch,
            "resource length changed after inventory validation",
        ));
    }
    let canonical_root = std::fs::canonicalize(root).map_err(|_| {
        ServingError::new(
            ServingErrorKind::ResourceDenied,
            "skill root cannot be resolved safely",
        )
    })?;
    let canonical = std::fs::canonicalize(&path).map_err(|_| {
        ServingError::new(
            ServingErrorKind::ResourceNotFound,
            "resource cannot be resolved",
        )
    })?;
    if !canonical.starts_with(canonical_root) {
        return Err(ServingError::new(
            ServingErrorKind::ResourceDenied,
            "resource escapes its skill root",
        ));
    }
    let bytes = std::fs::read(canonical).map_err(|_| {
        ServingError::new(
            ServingErrorKind::ResourceNotFound,
            "resource cannot be read",
        )
    })?;
    if super::sha256_hex(&bytes) != descriptor.sha256 {
        return Err(ServingError::new(
            ServingErrorKind::IntegrityMismatch,
            "resource hash changed after inventory validation",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        ServingError::new(
            ServingErrorKind::ResourceDenied,
            "resource is not UTF-8 text",
        )
    })
}

fn document_tokens(state: &ServingState) -> Vec<String> {
    let mut tokens = tokenize(&state.name);
    tokens.extend(tokenize(&state.description));
    tokens
}

/// Build the combined PageIndex corpus for one eligible skill.
///
/// Layers: (1) YAML frontmatter via `parse_source`, (2) SKILL.md body
/// headings/sections after frontmatter strip, (3) inventory-approved text resources.
fn build_skill_pageindex(state: &ServingState) -> Vec<SkillNavNode> {
    let Some(main) = state.main.as_ref() else {
        return Vec::new();
    };
    let Ok(raw) = read_verified_text(&state.source_dir, main) else {
        return Vec::new();
    };
    let parsed = crate::skills::frontmatter::parse_source(&raw);
    let mut nodes = Vec::new();

    push_frontmatter_node(&mut nodes, state, &parsed);
    push_markdown_document(
        &mut nodes,
        &state.skill_id,
        "SKILL.md",
        parsed.body,
        &state.name,
        SkillNavNodeKind::Document,
    );

    for (path, resource) in &state.resources {
        let Ok(content) = read_verified_text(&state.source_dir, resource) else {
            continue;
        };
        if is_markdown_media(&resource.media_type, path) {
            // Markdown resources get a Resource root + optional section children.
            push_markdown_document(
                &mut nodes,
                &state.skill_id,
                path,
                &content,
                &state.name,
                SkillNavNodeKind::Resource,
            );
            continue;
        }
        let stem = path_file_stem(path);
        let search_text = format!("{} {} {} {}", path, stem, state.name, content);
        nodes.push(SkillNavNode {
            skill_id: state.skill_id.clone(),
            node_id: path.clone(),
            kind: SkillNavNodeKind::Resource,
            path: path.clone(),
            heading: None,
            heading_level: None,
            parent_node_id: None,
            child_count: 0,
            lede: lede_text(&search_text),
            tokens: tokenize(&search_text),
        });
    }

    fill_child_counts(&mut nodes);
    nodes
}

fn push_frontmatter_node(
    nodes: &mut Vec<SkillNavNode>,
    state: &ServingState,
    parsed: &crate::skills::frontmatter::ParsedSource<'_>,
) {
    let mut parts = Vec::new();
    let name = parsed
        .name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(state.name.as_str());
    parts.push(name);
    if let Some(description) = parsed
        .description
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(description);
    } else if !state.description.is_empty() {
        parts.push(state.description.as_str());
    }
    if let Some(role) = parsed.role {
        parts.push(skill_role_token(role));
    }
    let search_text = parts.join(" ");
    nodes.push(SkillNavNode {
        skill_id: state.skill_id.clone(),
        node_id: "frontmatter".to_string(),
        kind: SkillNavNodeKind::Frontmatter,
        path: "SKILL.md".to_string(),
        heading: None,
        heading_level: None,
        parent_node_id: None,
        child_count: 0,
        lede: lede_text(&search_text),
        tokens: tokenize(&search_text),
    });
}

fn push_markdown_document(
    nodes: &mut Vec<SkillNavNode>,
    skill_id: &str,
    path: &str,
    body: &str,
    skill_name: &str,
    root_kind: SkillNavNodeKind,
) {
    let headings = collect_atx_headings(body);
    let stem = path_file_stem(path);
    let preamble = headings
        .first()
        .map(|heading| &body[..heading.line_start])
        .unwrap_or(body);
    let document_search = if headings.is_empty() {
        format!("{path} {stem} {skill_name} {body}")
    } else {
        format!("{path} {stem} {skill_name} {preamble}")
    };
    let document_node_id = path.to_string();
    nodes.push(SkillNavNode {
        skill_id: skill_id.to_string(),
        node_id: document_node_id.clone(),
        kind: root_kind,
        path: path.to_string(),
        heading: None,
        heading_level: None,
        parent_node_id: None,
        child_count: 0,
        lede: lede_text(&document_search),
        tokens: tokenize(&document_search),
    });

    // Stack of (heading_level, node_id) for parent linkage.
    let mut stack: Vec<(u8, String)> = Vec::new();
    for (index, heading) in headings.iter().enumerate() {
        while stack
            .last()
            .is_some_and(|(level, _)| *level >= heading.level)
        {
            stack.pop();
        }
        let parent_node_id = stack
            .last()
            .map(|(_, node_id)| node_id.clone())
            .unwrap_or_else(|| document_node_id.clone());
        let content_end = headings
            .get(index + 1)
            .map(|next| next.line_start)
            .unwrap_or(body.len());
        let section_body = body.get(heading.content_start..content_end).unwrap_or("");
        let search_text = format!("{skill_name} {} {section_body}", heading.title);
        let node_id = format!("{path}#s{index}");
        nodes.push(SkillNavNode {
            skill_id: skill_id.to_string(),
            node_id: node_id.clone(),
            kind: SkillNavNodeKind::Section,
            path: path.to_string(),
            heading: Some(heading.title.clone()),
            heading_level: Some(heading.level),
            parent_node_id: Some(parent_node_id),
            child_count: 0,
            lede: lede_text(&search_text),
            tokens: tokenize(&search_text),
        });
        stack.push((heading.level, node_id));
    }
}

#[derive(Debug)]
struct AtxHeading {
    level: u8,
    title: String,
    line_start: usize,
    content_start: usize,
}

fn collect_atx_headings(body: &str) -> Vec<AtxHeading> {
    let mut headings = Vec::new();
    let mut in_fence = false;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let fence_marker = trimmed
            .trim_start()
            .strip_prefix("```")
            .or_else(|| trimmed.trim_start().strip_prefix("~~~"));
        if fence_marker.is_some() {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some((level, title)) = parse_atx_heading_line(trimmed) {
            headings.push(AtxHeading {
                level,
                title,
                line_start,
                content_start: offset,
            });
        }
    }
    headings
}

fn parse_atx_heading_line(line: &str) -> Option<(u8, String)> {
    let bytes = line.as_bytes();
    let mut hash_count = 0_usize;
    while hash_count < bytes.len() && bytes[hash_count] == b'#' {
        hash_count += 1;
    }
    if hash_count == 0 || hash_count > 6 {
        return None;
    }
    if hash_count < bytes.len() && bytes[hash_count] != b' ' && bytes[hash_count] != b'\t' {
        return None;
    }
    let title = line[hash_count..]
        .trim()
        .trim_end_matches(|ch: char| ch == '#' || ch.is_whitespace())
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    Some((u8::try_from(hash_count).unwrap_or(6), title))
}

fn fill_child_counts(nodes: &mut [SkillNavNode]) {
    let mut counts = BTreeMap::<String, usize>::new();
    for node in nodes.iter() {
        if let Some(parent) = &node.parent_node_id {
            *counts.entry(parent.clone()).or_default() += 1;
        }
    }
    for node in nodes.iter_mut() {
        node.child_count = counts.get(&node.node_id).copied().unwrap_or(0);
    }
}

fn lede_text(value: &str) -> String {
    value.chars().take(LEDE_CHARS).collect()
}

fn path_file_stem(path: &str) -> &str {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(path)
}

fn is_markdown_media(media_type: &str, path: &str) -> bool {
    media_type == "text/markdown"
        || path.ends_with(".md")
        || path.ends_with(".markdown")
        || path.ends_with(".MD")
}

fn skill_role_token(role: crate::skills::SkillRole) -> &'static str {
    match role {
        crate::skills::SkillRole::Brain => "brain",
        crate::skills::SkillRole::Worker => "worker",
        crate::skills::SkillRole::Both => "both",
    }
}

fn tokenize(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn bm25_scores(states: &[ServingState], candidates: &[usize], query_tokens: &[String]) -> Vec<f64> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let documents: Vec<_> = candidates
        .iter()
        .map(|index| document_tokens(&states[*index]))
        .collect();
    let document_count = usize_as_f64(documents.len());
    let average_length = if documents.is_empty() {
        1.0
    } else {
        documents
            .iter()
            .map(Vec::len)
            .map(usize_as_f64)
            .sum::<f64>()
            / document_count
    };
    documents
        .iter()
        .map(|document| {
            let document_length = usize_as_f64(document.len());
            query_tokens
                .iter()
                .map(|term| {
                    let term_frequency = document.iter().filter(|token| *token == term).count();
                    if term_frequency == 0 {
                        return 0.0;
                    }
                    let term_frequency = usize_as_f64(term_frequency);
                    let document_frequency = usize_as_f64(
                        documents
                            .iter()
                            .filter(|candidate| candidate.contains(term))
                            .count(),
                    );
                    let inverse_document_frequency = ((document_count - document_frequency + 0.5)
                        / (document_frequency + 0.5))
                        .ln_1p();
                    let normalization =
                        term_frequency + K1 * (1.0 - B + B * document_length / average_length);
                    inverse_document_frequency * term_frequency * (K1 + 1.0) / normalization
                })
                .sum()
        })
        .collect()
}

fn usize_as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn identity_sort_key(identity: &VersionIdentity) -> String {
    format!(
        "{}\0{}\0{}\0{}",
        identity.source, identity.rel_path, identity.pinned_commit, identity.content_sha256
    )
}

fn lineage_sha256(identity: &VersionIdentity) -> String {
    super::sha256_hex(format!("{}\0{}", identity.source, identity.rel_path).as_bytes())
}

fn encode_skill_ref(identity: &VersionIdentity) -> String {
    SkillReference {
        lineage_sha256: lineage_sha256(identity),
        version_sha256: super::sha256_hex(identity_sort_key(identity).as_bytes()),
    }
    .encode()
}

fn decode_skill_ref(skill_id: &str) -> Option<SkillReference> {
    let digests = skill_id.strip_prefix("skillref.v1.")?;
    let (lineage_sha256, version_sha256) = digests.split_once('.')?;
    let is_lower_hex = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if !is_lower_hex(lineage_sha256) || !is_lower_hex(version_sha256) {
        return None;
    }
    Some(SkillReference {
        lineage_sha256: lineage_sha256.to_owned(),
        version_sha256: version_sha256.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decode_skill_ref, evaluate_eligibility, ContextCompatibility, Eligibility,
        EligibilityInput, ServingCatalog, ServingErrorKind, SkillNavNodeKind,
        MAX_TEXT_CONTENT_BYTES,
    };
    use crate::explore::catalog::{Catalog, CatalogEntry, ItemKind};
    use crate::explore::pool::{GateRecord, Manifest, ManifestItem};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[derive(Debug, Clone, Copy)]
    enum Layer {
        Local,
        Global,
    }

    struct TestWorld {
        repo: tempfile::TempDir,
        global: tempfile::TempDir,
        bundled: tempfile::TempDir,
    }

    impl TestWorld {
        fn new() -> Self {
            Self {
                repo: tempfile::tempdir().unwrap(),
                global: tempfile::tempdir().unwrap(),
                bundled: tempfile::tempdir().unwrap(),
            }
        }

        fn load(&self) -> ServingCatalog {
            ServingCatalog::load_from_roots(
                self.repo.path(),
                self.bundled.path(),
                Some(self.global.path()),
            )
            .unwrap()
        }

        fn bundled_skill(&self, name: &str, description: &str) -> PathBuf {
            let dir = self.bundled.path().join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\n"),
            )
            .unwrap();
            dir
        }

        fn pool_skill(
            &self,
            layer: Layer,
            name: &str,
            source: &str,
            pinned_commit: &str,
            description: &str,
            verdict: Option<&str>,
        ) -> PathBuf {
            let store_root = self.store_root(layer);
            let dir =
                crate::explore::pool::pool_dir_in_store(&store_root, source, name, pinned_commit);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {description}\n---\n# {name}\n"),
            )
            .unwrap();
            let content_sha256 = crate::explore::content_hash(&dir).unwrap();
            let entry = CatalogEntry {
                kind: ItemKind::Skill,
                name: name.to_string(),
                source: source.to_string(),
                rel_path: format!("skills/{name}"),
                pinned_commit: pinned_commit.to_string(),
                description: description.to_string(),
                license: None,
                content_sha256,
            };
            self.upsert_catalog(layer, entry.clone());
            if let Some(verdict) = verdict {
                self.upsert_manifest(layer, &entry, verdict);
            }
            dir
        }

        fn refresh_hash(&self, layer: Layer, name: &str) {
            let mut catalog = self.catalog(layer);
            let entry = catalog
                .entries
                .iter_mut()
                .find(|entry| entry.name == name)
                .unwrap();
            let dir = crate::explore::pool::pool_dir_in_store(
                &self.store_root(layer),
                &entry.source,
                &entry.name,
                &entry.pinned_commit,
            );
            entry.content_sha256 = crate::explore::content_hash(&dir).unwrap();
            let updated = entry.clone();
            self.save_catalog(layer, &catalog);
            let mut manifest = self.manifest(layer);
            if let Some(item) = manifest.items.iter_mut().find(|item| item.name == name) {
                item.content_sha256.clone_from(&updated.content_sha256);
            }
            self.save_manifest(layer, &manifest);
        }

        fn set_verdict(&self, layer: Layer, name: &str, verdict: &str) {
            let mut manifest = self.manifest(layer);
            manifest
                .items
                .iter_mut()
                .find(|item| item.name == name)
                .unwrap()
                .gate
                .verdict = verdict.to_string();
            self.save_manifest(layer, &manifest);
        }

        fn store_root(&self, layer: Layer) -> PathBuf {
            match layer {
                Layer::Local => crate::explore::store::local_root(self.repo.path()),
                Layer::Global => self.global.path().to_path_buf(),
            }
        }

        fn catalog(&self, layer: Layer) -> Catalog {
            match layer {
                Layer::Local => Catalog::load(self.repo.path()).unwrap(),
                Layer::Global => Catalog::load_from_store(self.global.path()).unwrap(),
            }
        }

        fn save_catalog(&self, layer: Layer, catalog: &Catalog) {
            match layer {
                Layer::Local => catalog.save(self.repo.path()).unwrap(),
                Layer::Global => catalog.save_to_store(self.global.path()).unwrap(),
            }
        }

        fn manifest(&self, layer: Layer) -> Manifest {
            match layer {
                Layer::Local => Manifest::load(self.repo.path()).unwrap(),
                Layer::Global => Manifest::load_from_store(self.global.path()).unwrap(),
            }
        }

        fn save_manifest(&self, layer: Layer, manifest: &Manifest) {
            match layer {
                Layer::Local => manifest.save(self.repo.path()).unwrap(),
                Layer::Global => manifest.save_to_store(self.global.path()).unwrap(),
            }
        }

        fn upsert_catalog(&self, layer: Layer, entry: CatalogEntry) {
            let mut catalog = self.catalog(layer);
            catalog
                .entries
                .retain(|existing| existing.name != entry.name);
            catalog.entries.push(entry);
            self.save_catalog(layer, &catalog);
        }

        fn upsert_manifest(&self, layer: Layer, entry: &CatalogEntry, verdict: &str) {
            let mut manifest = self.manifest(layer);
            manifest
                .items
                .retain(|existing| existing.name != entry.name);
            manifest.items.push(ManifestItem {
                name: entry.name.clone(),
                kind: entry.kind,
                source: entry.source.clone(),
                rel_path: entry.rel_path.clone(),
                pinned_commit: entry.pinned_commit.clone(),
                content_sha256: entry.content_sha256.clone(),
                license: entry.license.clone(),
                gate: GateRecord {
                    verdict: verdict.to_string(),
                    justification: None,
                    decided_at_epoch: None,
                },
            });
            self.save_manifest(layer, &manifest);
        }
    }

    #[test]
    fn pageindex_indexes_frontmatter_tokens_including_folded_description() {
        let world = TestWorld::new();
        let dir = world.bundled.path().join("folded-skill");
        fs::create_dir_all(&dir).unwrap();
        // Avoid `\` line-continuations: they strip indentation required by folded YAML.
        fs::write(
            dir.join("SKILL.md"),
            concat!(
                "---\n",
                "name: folded-skill\n",
                "description: >\n",
                "  Use when hunting rarefrontmattertoken across sessions.\n",
                "  Keeps implementation honest.\n",
                "role: worker\n",
                "---\n",
                "# Folded Skill\n",
                "\n",
                "Section body carries sectiononlytoken for navigation.\n",
            ),
        )
        .unwrap();

        let catalog = world.load();
        let state = catalog
            .states
            .iter()
            .find(|state| state.name == "folded-skill")
            .expect("eligible skill state");
        assert_eq!(state.eligibility, Eligibility::Eligible);
        assert!(!state.nodes.is_empty());

        let frontmatter = state
            .nodes
            .iter()
            .find(|node| node.kind == SkillNavNodeKind::Frontmatter)
            .expect("frontmatter node");
        assert!(
            frontmatter
                .tokens
                .iter()
                .any(|token| token == "rarefrontmattertoken"),
            "folded description must contribute frontmatter tokens: {:?}",
            frontmatter.tokens
        );
        assert!(
            frontmatter.tokens.iter().any(|token| token == "worker"),
            "role must contribute frontmatter tokens: {:?}",
            frontmatter.tokens
        );
        assert!(
            frontmatter.lede.contains("rarefrontmattertoken"),
            "lede should surface frontmatter text: {}",
            frontmatter.lede
        );

        let section = state
            .nodes
            .iter()
            .find(|node| node.kind == SkillNavNodeKind::Section)
            .expect("section node");
        assert!(
            section
                .tokens
                .iter()
                .any(|token| token == "sectiononlytoken"),
            "section tokens must include body: {:?}",
            section.tokens
        );
        assert!(
            !section
                .tokens
                .iter()
                .any(|token| token == "rarefrontmattertoken"),
            "section tokens must not include frontmatter-only description: {:?}",
            section.tokens
        );
        assert!(
            !section.tokens.iter().any(|token| token == "description"),
            "section body must not index the YAML frontmatter block: {:?}",
            section.tokens
        );
        assert_eq!(section.heading.as_deref(), Some("Folded Skill"));
        assert_eq!(section.heading_level, Some(1));
        assert_eq!(section.parent_node_id.as_deref(), Some("SKILL.md"));
        assert!(section.lede.chars().count() <= super::LEDE_CHARS);
    }

    #[test]
    fn pageindex_indexes_approved_markdown_resources_and_excludes_scripts() {
        let world = TestWorld::new();
        let dir = world.bundled_skill("resource-nav", "resource navigation workflow");
        fs::create_dir_all(dir.join("references")).unwrap();
        fs::write(
            dir.join("references/guide.md"),
            "# Resource Guide\n\nBody carries resourcemdonlytoken for FTS.\n",
        )
        .unwrap();
        // scripts would make the skill ineligible under inventory rules; model that
        // path as denied so PageIndex never indexes script content.
        let scripted = world.bundled_skill("scripted-nav", "scripted navigation workflow");
        fs::create_dir_all(scripted.join("scripts")).unwrap();
        fs::write(
            scripted.join("scripts/run.sh"),
            "#!/bin/sh\necho scriptonlytoken\n",
        )
        .unwrap();

        let catalog = world.load();

        let resource_state = catalog
            .states
            .iter()
            .find(|state| state.name == "resource-nav")
            .expect("resource skill");
        assert_eq!(resource_state.eligibility, Eligibility::Eligible);
        let resource_node = resource_state
            .nodes
            .iter()
            .find(|node| {
                node.kind == SkillNavNodeKind::Resource && node.path == "references/guide.md"
            })
            .expect("resource document node");
        assert_eq!(resource_node.node_id, "references/guide.md");
        let resource_section = resource_state
            .nodes
            .iter()
            .find(|node| {
                node.kind == SkillNavNodeKind::Section && node.path == "references/guide.md"
            })
            .expect("resource section");
        assert!(
            resource_section
                .tokens
                .iter()
                .any(|token| token == "resourcemdonlytoken"),
            "approved .md resource body must be indexed: {:?}",
            resource_section.tokens
        );
        assert!(
            resource_state
                .nodes
                .iter()
                .any(|node| node.kind == SkillNavNodeKind::Frontmatter),
            "eligible skills still get a frontmatter node"
        );

        let scripted_state = catalog
            .states
            .iter()
            .find(|state| state.name == "scripted-nav")
            .expect("scripted skill state");
        assert_eq!(
            scripted_state.compatibility,
            ContextCompatibility::RequiresScripts
        );
        assert_eq!(scripted_state.eligibility, Eligibility::Ineligible);
        assert!(
            scripted_state.nodes.is_empty(),
            "ineligible scripted skills must not build a PageIndex"
        );
        assert!(
            !scripted_state
                .nodes
                .iter()
                .any(|node| node.tokens.iter().any(|token| token == "scriptonlytoken")),
            "script content must never be indexed"
        );
    }

    #[test]
    fn pageindex_section_search_text_excludes_yaml_frontmatter_block() {
        let world = TestWorld::new();
        let dir = world.bundled.path().join("guard-skill");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            concat!(
                "---\n",
                "name: guard-skill\n",
                "description: yamlblockonlytoken lives only in frontmatter\n",
                "---\n",
                "# After Frontmatter\n",
                "\n",
                "Body uses bodyonlytoken and must not re-index the YAML fence.\n",
            ),
        )
        .unwrap();

        let catalog = world.load();
        let state = catalog
            .states
            .iter()
            .find(|state| state.name == "guard-skill")
            .expect("guard-skill skill");
        let frontmatter = state
            .nodes
            .iter()
            .find(|node| node.kind == SkillNavNodeKind::Frontmatter)
            .expect("frontmatter");
        assert!(frontmatter
            .tokens
            .iter()
            .any(|token| token == "yamlblockonlytoken"));
        let section = state
            .nodes
            .iter()
            .find(|node| node.kind == SkillNavNodeKind::Section)
            .expect("section");
        assert!(section.tokens.iter().any(|token| token == "bodyonlytoken"));
        assert!(!section
            .tokens
            .iter()
            .any(|token| token == "yamlblockonlytoken"));
        assert!(!section.lede.contains("---"));
        assert!(!section.lede.contains("description:"));
        let document = state
            .nodes
            .iter()
            .find(|node| node.kind == SkillNavNodeKind::Document)
            .expect("document node");
        assert!(!document
            .tokens
            .iter()
            .any(|token| token == "yamlblockonlytoken"));
        assert!(!document.lede.contains("description:"));
    }

    #[test]
    fn eligibility_matches_the_complete_policy_truth_table() {
        for mask in 0_u8..32 {
            let input = EligibilityInput {
                bundled: mask & 1 != 0,
                adopted: mask & 2 != 0,
                gate_approved: mask & 4 != 0,
                enabled: mask & 8 != 0,
                compatible: mask & 16 != 0,
            };
            let expected = if input.enabled
                && input.compatible
                && (input.bundled || (input.adopted && input.gate_approved))
            {
                Eligibility::Eligible
            } else {
                Eligibility::Ineligible
            };

            assert_eq!(evaluate_eligibility(input), expected, "mask {mask:05b}");
        }
    }

    #[test]
    fn serving_view_includes_only_bundled_or_adopted_approved_compatible_skills() {
        let world = TestWorld::new();
        world.bundled_skill("bundled-review", "bundled review workflow");
        world.pool_skill(
            Layer::Local,
            "approved-api",
            "acme/skills",
            &"a".repeat(40),
            "approved api workflow",
            Some("clean"),
        );
        world.pool_skill(
            Layer::Local,
            "synced-only",
            "acme/skills",
            &"b".repeat(40),
            "synced only workflow",
            None,
        );
        world.pool_skill(
            Layer::Local,
            "rejected-skill",
            "acme/skills",
            &"c".repeat(40),
            "rejected workflow",
            Some("flagged"),
        );
        world.pool_skill(
            Layer::Local,
            "disabled-skill",
            "acme/skills",
            &"d".repeat(40),
            "disabled workflow",
            Some("disabled"),
        );
        let incompatible = world.bundled_skill("scripted-skill", "scripted workflow");
        fs::create_dir_all(incompatible.join("scripts")).unwrap();
        fs::write(incompatible.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();

        let catalog = world.load();

        assert_eq!(
            catalog
                .search("bundled review", None, None)
                .unwrap()
                .results
                .len(),
            1
        );
        assert_eq!(
            catalog
                .search("approved api", None, None)
                .unwrap()
                .results
                .len(),
            1
        );
        for hidden in ["synced only", "rejected", "disabled", "scripted"] {
            assert!(catalog
                .search(hidden, None, None)
                .unwrap()
                .results
                .is_empty());
        }
        assert_eq!(
            catalog
                .decision("scripted-skill", "bundled")
                .unwrap()
                .compatibility,
            ContextCompatibility::RequiresScripts
        );
    }

    #[test]
    fn unapproved_local_shadow_does_not_replace_approved_global_skill() {
        let world = TestWorld::new();
        world.pool_skill(
            Layer::Global,
            "secure-review",
            "trusted/global",
            &"a".repeat(40),
            "trusted global security review",
            Some("clean"),
        );
        world.pool_skill(
            Layer::Local,
            "secure-review",
            "untrusted/local",
            &"b".repeat(40),
            "unapproved local shadow",
            Some("flagged"),
        );

        let response = world.load().search("secure review", None, None).unwrap();

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].source, "trusted/global");
        assert_eq!(
            response.results[0].description,
            "trusted global security review"
        );
    }

    #[test]
    fn unapproved_same_identity_local_copy_preserves_global_approval() {
        let world = TestWorld::new();
        let commit = "a".repeat(40);
        for (layer, verdict) in [(Layer::Global, "clean"), (Layer::Local, "flagged")] {
            world.pool_skill(
                layer,
                "same-identity",
                "trusted/skills",
                &commit,
                "same pinned content",
                Some(verdict),
            );
        }

        let response = world.load().search("same identity", None, None).unwrap();

        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].source, "trusted/skills");
    }

    #[test]
    fn unsafe_catalog_pool_components_are_incompatible_without_being_read() {
        let world = TestWorld::new();
        let entry = CatalogEntry {
            kind: ItemKind::Skill,
            name: "/absolute-skill".to_string(),
            source: "acme/skills".to_string(),
            rel_path: "skills/absolute-skill".to_string(),
            pinned_commit: "a".repeat(40),
            description: "unsafe pool path".to_string(),
            license: None,
            content_sha256: "0".repeat(64),
        };
        world.upsert_catalog(Layer::Local, entry.clone());
        world.upsert_manifest(Layer::Local, &entry, "clean");

        let catalog = world.load();

        assert_eq!(
            catalog
                .decision("/absolute-skill", "acme/skills")
                .unwrap()
                .compatibility,
            ContextCompatibility::UnsafeResourcePath
        );
        assert!(catalog
            .search("unsafe pool", None, None)
            .unwrap()
            .results
            .is_empty());
    }

    #[test]
    fn search_is_deterministic_bounded_filterable_and_exact_token_first() {
        let world = TestWorld::new();
        world.bundled_skill("auth", "generic validation workflow");
        world.bundled_skill("auth-review", "authentication review workflow");
        world.bundled_skill("alpha-check", "verification workflow");
        world.bundled_skill("beta-check", "verification workflow");
        for suffix in ["one", "two", "three", "four", "five", "six"] {
            world.bundled_skill(&format!("review-{suffix}"), "review workflow");
        }
        world.pool_skill(
            Layer::Local,
            "external-auth",
            "acme/skills",
            &"e".repeat(40),
            "external authentication workflow",
            Some("clean"),
        );
        let catalog = world.load();

        let exact = catalog.search("auth", None, None).unwrap();
        assert_eq!(exact.results[0].name, "auth");
        let ties = catalog.search("verification", None, None).unwrap();
        assert_eq!(ties.results[0].name, "alpha-check");
        assert_eq!(ties.results[1].name, "beta-check");
        assert_eq!(
            catalog.search("review", None, None).unwrap().results.len(),
            5
        );
        let filtered = catalog
            .search("authentication", Some(1), Some("acme/skills"))
            .unwrap();
        assert_eq!(filtered.results[0].name, "external-auth");
        assert_eq!(
            catalog.search("verification", Some(2), None).unwrap(),
            catalog.search("verification", Some(2), None).unwrap()
        );
        for limit in [Some(0), Some(6)] {
            assert_eq!(
                catalog.search("review", limit, None).unwrap_err().kind(),
                ServingErrorKind::InvalidQuery
            );
        }
        assert_eq!(
            catalog.search("   ", None, None).unwrap_err().kind(),
            ServingErrorKind::InvalidQuery
        );
    }

    #[test]
    fn search_prefers_name_token_over_description_only_match() {
        let world = TestWorld::new();
        world.bundled_skill("alpha-helper", "audit workflow");
        world.bundled_skill("zeta-audit", "generic workflow");
        let catalog = world.load();

        let response = catalog.search("audit", None, None).unwrap();

        assert_eq!(response.results[0].name, "zeta-audit");
        assert_eq!(response.results[1].name, "alpha-helper");
    }

    #[test]
    fn opaque_refs_round_trip_and_reads_reject_stale_or_revoked_versions() {
        let world = TestWorld::new();
        world.pool_skill(
            Layer::Local,
            "versioned-skill",
            "acme/skills",
            &"a".repeat(40),
            "versioned workflow",
            Some("clean"),
        );
        let catalog = world.load();
        let hit = catalog
            .search("versioned", None, None)
            .unwrap()
            .results
            .remove(0);
        let reference = decode_skill_ref(&hit.skill_id).unwrap();
        assert_eq!(reference.encode(), hit.skill_id);
        assert!(!hit.skill_id.contains("acme"));
        assert!(!hit.skill_id.contains("versioned-skill"));
        assert!(catalog
            .read(&hit.skill_id, None)
            .unwrap()
            .content
            .contains("# versioned-skill"));
        assert_eq!(
            catalog.read("not-an-opaque-ref", None).unwrap_err().kind(),
            ServingErrorKind::SkillNotFound
        );

        world.pool_skill(
            Layer::Local,
            "versioned-skill",
            "acme/skills",
            &"b".repeat(40),
            "versioned workflow two",
            Some("clean"),
        );
        assert_eq!(
            catalog.read(&hit.skill_id, None).unwrap_err().kind(),
            ServingErrorKind::StaleSkillRef
        );

        let current = world
            .load()
            .search("versioned", None, None)
            .unwrap()
            .results
            .remove(0);
        world.set_verdict(Layer::Local, "versioned-skill", "disabled");
        assert_eq!(
            catalog.read(&current.skill_id, None).unwrap_err().kind(),
            ServingErrorKind::SkillNotEligible
        );

        world.set_verdict(Layer::Local, "versioned-skill", "clean");
        let removable_catalog = world.load();
        let removable = removable_catalog
            .search("versioned", None, None)
            .unwrap()
            .results
            .remove(0);
        let mut stored_catalog = world.catalog(Layer::Local);
        stored_catalog
            .entries
            .retain(|entry| entry.name != "versioned-skill");
        world.save_catalog(Layer::Local, &stored_catalog);
        let mut stored_manifest = world.manifest(Layer::Local);
        stored_manifest
            .items
            .retain(|item| item.name != "versioned-skill");
        world.save_manifest(Layer::Local, &stored_manifest);
        assert_eq!(
            removable_catalog
                .read(&removable.skill_id, None)
                .unwrap_err()
                .kind(),
            ServingErrorKind::SkillNotEligible
        );
    }

    #[test]
    fn read_confines_resources_and_rechecks_integrity() {
        let world = TestWorld::new();
        let dir = world.pool_skill(
            Layer::Local,
            "resource-skill",
            "acme/skills",
            &"a".repeat(40),
            "resource workflow",
            Some("clean"),
        );
        fs::create_dir_all(dir.join("references")).unwrap();
        fs::write(dir.join("references/guide.md"), "approved guide\n").unwrap();
        world.refresh_hash(Layer::Local, "resource-skill");
        let catalog = world.load();
        let hit = catalog
            .search("resource", None, None)
            .unwrap()
            .results
            .remove(0);

        let guide = catalog
            .read(&hit.skill_id, Some("references/guide.md"))
            .unwrap();
        assert_eq!(guide.content, "approved guide\n");
        assert_eq!(guide.media_type, "text/markdown");
        for denied in ["/etc/passwd", "../other/SKILL.md", "references/../SKILL.md"] {
            assert_eq!(
                catalog
                    .read(&hit.skill_id, Some(denied))
                    .unwrap_err()
                    .kind(),
                ServingErrorKind::ResourceDenied
            );
        }
        assert_eq!(
            catalog
                .read(&hit.skill_id, Some("references/missing.md"))
                .unwrap_err()
                .kind(),
            ServingErrorKind::ResourceNotFound
        );

        fs::write(dir.join("SKILL.md"), "tampered").unwrap();
        assert_eq!(
            catalog.read(&hit.skill_id, None).unwrap_err().kind(),
            ServingErrorKind::IntegrityMismatch
        );
    }

    #[test]
    fn read_denies_script_and_unsupported_media_before_inventory_lookup() {
        let world = TestWorld::new();
        world.bundled_skill("resource-policy", "resource policy workflow");
        let catalog = world.load();
        let hit = catalog
            .search("resource policy", None, None)
            .unwrap()
            .results
            .remove(0);

        for denied in [
            "scripts/run.sh",
            "references/build.py",
            "references/payload.bin",
            "references/diagram.png",
        ] {
            assert_eq!(
                catalog
                    .read(&hit.skill_id, Some(denied))
                    .unwrap_err()
                    .kind(),
                ServingErrorKind::ResourceDenied,
                "requested path {denied}"
            );
        }
        assert_eq!(
            catalog
                .read(&hit.skill_id, Some("references/missing.md"))
                .unwrap_err()
                .kind(),
            ServingErrorKind::ResourceNotFound
        );
    }

    #[cfg(unix)]
    #[test]
    fn compatibility_classifies_scripts_binary_non_utf8_symlinks_and_size() {
        use std::os::unix::fs::symlink;

        let world = TestWorld::new();
        let script = world.bundled_skill("scripted", "scripted workflow");
        fs::create_dir_all(script.join("scripts")).unwrap();
        fs::write(script.join("scripts/run.sh"), "#!/bin/sh\n").unwrap();
        let binary = world.bundled_skill("binary", "binary workflow");
        fs::write(binary.join("image.png"), [0_u8, 159, 146, 150]).unwrap();
        let non_utf8 = world.bundled_skill("non-utf8", "non utf8 workflow");
        fs::write(non_utf8.join("notes.txt"), [0xff_u8, 0xfe]).unwrap();
        let oversized = world.bundled_skill("oversized", "oversized workflow");
        fs::write(
            oversized.join("large.md"),
            vec![b'x'; MAX_TEXT_CONTENT_BYTES + 1],
        )
        .unwrap();
        let linked = world.bundled_skill("linked", "linked workflow");
        symlink("SKILL.md", linked.join("guide.md")).unwrap();

        let catalog = world.load();
        for (name, expected) in [
            ("scripted", ContextCompatibility::RequiresScripts),
            ("binary", ContextCompatibility::BinaryResources),
            ("non-utf8", ContextCompatibility::NonUtf8Text),
            ("oversized", ContextCompatibility::ContentTooLarge),
            ("linked", ContextCompatibility::SymlinkResource),
        ] {
            assert_eq!(
                catalog.decision(name, "bundled").unwrap().compatibility,
                expected
            );
            assert!(catalog.search(name, None, None).unwrap().results.is_empty());
        }
        let oversized_state = catalog
            .states
            .iter()
            .find(|state| state.name == "oversized")
            .unwrap();
        assert_eq!(
            catalog
                .read(&oversized_state.skill_id, None)
                .unwrap_err()
                .kind(),
            ServingErrorKind::ContentTooLarge
        );
    }

    #[test]
    fn catalog_revision_is_order_independent_and_metadata_sensitive() {
        let world = TestWorld::new();
        for (name, commit) in [("alpha", "a"), ("beta", "b")] {
            world.pool_skill(
                Layer::Local,
                name,
                "acme/skills",
                &commit.repeat(40),
                &format!("{name} workflow"),
                Some("clean"),
            );
        }
        let first = world.load().revision().to_string();
        let mut catalog = world.catalog(Layer::Local);
        let mut manifest = world.manifest(Layer::Local);
        catalog.entries.reverse();
        manifest.items.reverse();
        world.save_catalog(Layer::Local, &catalog);
        world.save_manifest(Layer::Local, &manifest);
        assert_eq!(world.load().revision(), first);

        catalog.entries[0].description.push_str(" changed");
        world.save_catalog(Layer::Local, &catalog);
        assert_ne!(world.load().revision(), first);
    }

    #[test]
    fn search_and_read_do_not_write_to_any_catalog_root() {
        let world = TestWorld::new();
        world.bundled_skill("read-only", "read only workflow");
        world.pool_skill(
            Layer::Local,
            "pool-read-only",
            "acme/skills",
            &"a".repeat(40),
            "pool read only workflow",
            Some("clean"),
        );
        let before = [
            snapshot(world.repo.path()),
            snapshot(world.global.path()),
            snapshot(world.bundled.path()),
        ];

        let catalog = world.load();
        let hit = catalog
            .search("read only", None, None)
            .unwrap()
            .results
            .remove(0);
        catalog.read(&hit.skill_id, None).unwrap();

        let after = [
            snapshot(world.repo.path()),
            snapshot(world.global.path()),
            snapshot(world.bundled.path()),
        ];
        assert_eq!(after, before);
    }

    #[test]
    fn stable_error_kind_strings_match_the_mcp_contract() {
        assert_eq!(
            [
                ServingErrorKind::InvalidQuery,
                ServingErrorKind::SkillNotFound,
                ServingErrorKind::SkillNotEligible,
                ServingErrorKind::StaleSkillRef,
                ServingErrorKind::ResourceNotFound,
                ServingErrorKind::ResourceDenied,
                ServingErrorKind::ContentTooLarge,
                ServingErrorKind::IntegrityMismatch,
            ]
            .map(ServingErrorKind::as_str),
            [
                "invalid_query",
                "skill_not_found",
                "skill_not_eligible",
                "stale_skill_ref",
                "resource_not_found",
                "resource_denied",
                "content_too_large",
                "integrity_mismatch",
            ]
        );
    }

    fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
            let mut entries = fs::read_dir(dir)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(fs::DirEntry::file_name);
            for entry in entries {
                let path = entry.path();
                let file_type = entry.file_type().unwrap();
                if file_type.is_dir() {
                    visit(root, &path, files);
                } else if file_type.is_file() {
                    files.insert(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }

        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }
}
