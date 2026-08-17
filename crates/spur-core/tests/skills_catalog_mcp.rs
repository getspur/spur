use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spur_acp::config::{SkillsProjectionMode, SpurConfig};
use spur_core::{
    explore::{
        catalog::{Catalog, CatalogEntry, ItemKind},
        content_hash,
        pool::{GateRecord, Manifest, ManifestItem},
        serving::{
            ContextCompatibility, ServingCatalog, ServingErrorKind, MAX_SEARCH_LIMIT,
            MAX_TEXT_CONTENT_BYTES,
        },
        store,
    },
    mcp::skills_catalog::SkillsCatalogMcpModule,
    skills::{
        adapters::Adapter,
        projection::{resolver::resolve_effective_skills, RuntimeRole, SelectionPolicy},
    },
};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};

const BOOTSTRAP_SKILL: &str = include_str!("../../spur-cli/assets/skills/skills-catalog/SKILL.md");
const RETRIEVAL_FIXTURE: &str = include_str!("fixtures/skills_catalog_queries.json");
const NAVIGATE_FIXTURE: &str = include_str!("fixtures/skills_navigate_queries.json");

#[derive(Debug, Clone, Copy)]
enum Layer {
    Local,
    Global,
}

struct PoolSkillSpec<'a> {
    name: &'a str,
    source: &'a str,
    pinned_commit: &'a str,
    description: &'a str,
    verdict: &'a str,
    body: &'a str,
}

/// Serializes HOME/USERPROFILE mutation across parallel integration tests.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EnvironmentGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvironmentGuard {
    fn isolated_home(home: &Path) -> Self {
        // Hold the process-wide env lock for the full TestWorld lifetime so
        // parallel skills_catalog_mcp tests cannot clobber each other's HOME.
        let lock = env_lock();
        const HOME_KEYS: [&str; 2] = ["HOME", "USERPROFILE"];
        let saved = HOME_KEYS
            .into_iter()
            .map(|key| {
                let previous = std::env::var_os(key);
                // Serialized by env_lock across this test binary.
                std::env::set_var(key, home);
                (key, previous)
            })
            .collect();
        Self { _lock: lock, saved }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (key, previous) in self.saved.drain(..) {
            if let Some(previous) = previous {
                std::env::set_var(key, previous);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

struct TestWorld {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
    _environment: EnvironmentGuard,
}

impl TestWorld {
    fn new() -> Self {
        let repo = tempfile::tempdir().expect("temporary repository");
        let home = tempfile::tempdir().expect("temporary home");
        let environment = EnvironmentGuard::isolated_home(home.path());
        Self {
            repo,
            home,
            _environment: environment,
        }
    }

    fn repo_root(&self) -> &Path {
        self.repo.path()
    }

    fn global_root(&self) -> PathBuf {
        self.home.path().join(".spur/explore")
    }

    fn bundled_skill(&self, name: &str, description: &str, body: &str) -> PathBuf {
        let directory = self.repo.path().join("assets/skills").join(name);
        fs::create_dir_all(&directory).expect("create bundled skill directory");
        fs::write(
            directory.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\nrole: both\n---\n\n# {name}\n\n{body}\n"
            ),
        )
        .expect("write bundled skill");
        directory
    }

    fn pool_skill(&self, layer: Layer, spec: PoolSkillSpec<'_>) -> PathBuf {
        let PoolSkillSpec {
            name,
            source,
            pinned_commit,
            description,
            verdict,
            body,
        } = spec;
        let store_root = self.store_root(layer);
        let directory =
            spur_core::explore::pool::pool_dir_in_store(&store_root, source, name, pinned_commit);
        fs::create_dir_all(&directory).expect("create pool skill directory");
        fs::write(
            directory.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\nrole: both\n---\n\n# {name}\n\n{body}\n"
            ),
        )
        .expect("write pool skill");
        let entry = CatalogEntry {
            kind: ItemKind::Skill,
            name: name.to_owned(),
            source: source.to_owned(),
            rel_path: format!("skills/{name}"),
            pinned_commit: pinned_commit.to_owned(),
            description: description.to_owned(),
            license: None,
            content_sha256: content_hash(&directory).expect("hash pool skill"),
        };
        self.upsert_catalog(layer, entry.clone());
        self.upsert_manifest(layer, &entry, verdict);
        directory
    }

    fn set_verdict(&self, layer: Layer, name: &str, verdict: &str) {
        let mut manifest = self.manifest(layer);
        manifest
            .items
            .iter_mut()
            .find(|item| item.name == name)
            .expect("manifest item")
            .gate
            .verdict = verdict.to_owned();
        self.save_manifest(layer, &manifest);
    }

    fn store_root(&self, layer: Layer) -> PathBuf {
        match layer {
            Layer::Local => store::local_root(self.repo.path()),
            Layer::Global => self.global_root(),
        }
    }

    fn catalog(&self, layer: Layer) -> Catalog {
        match layer {
            Layer::Local => Catalog::load(self.repo.path()),
            Layer::Global => Catalog::load_from_store(&self.global_root()),
        }
        .expect("load catalog")
    }

    fn save_catalog(&self, layer: Layer, catalog: &Catalog) {
        match layer {
            Layer::Local => catalog.save(self.repo.path()),
            Layer::Global => catalog.save_to_store(&self.global_root()),
        }
        .expect("save catalog");
    }

    fn manifest(&self, layer: Layer) -> Manifest {
        match layer {
            Layer::Local => Manifest::load(self.repo.path()),
            Layer::Global => Manifest::load_from_store(&self.global_root()),
        }
        .expect("load manifest")
    }

    fn save_manifest(&self, layer: Layer, manifest: &Manifest) {
        match layer {
            Layer::Local => manifest.save(self.repo.path()),
            Layer::Global => manifest.save_to_store(&self.global_root()),
        }
        .expect("save manifest");
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
                verdict: verdict.to_owned(),
                justification: None,
                decided_at_epoch: None,
            },
        });
        self.save_manifest(layer, &manifest);
    }
}

#[derive(Debug, Deserialize)]
struct RetrievalFixture {
    schema_version: u32,
    k: usize,
    cases: Vec<RetrievalCase>,
}

#[derive(Debug, Deserialize)]
struct RetrievalCase {
    id: String,
    query: String,
    #[serde(default)]
    source: Option<String>,
    acceptable_skill_ids: Vec<String>,
    #[serde(default)]
    expected_source: Option<String>,
    expected_zero_results: bool,
    #[serde(default)]
    refinement: Option<RefinementCase>,
}

#[derive(Debug, Deserialize)]
struct RefinementCase {
    query: String,
    acceptable_skill_ids: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct RetrievalMetrics {
    evaluated_queries: usize,
    #[serde(rename = "recall@5")]
    recall_at_5: String,
    #[serde(rename = "precision@5")]
    precision_at_5: String,
    mrr: String,
    zero_result_rate: String,
    refinement_recovery: String,
}

fn fixed_rate(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        return "n/a".to_owned();
    }
    format!("{:.3}", numerator as f64 / denominator as f64)
}

fn evaluate_retrieval(catalog: &ServingCatalog, fixture: &RetrievalFixture) -> RetrievalMetrics {
    assert_eq!(fixture.schema_version, 1, "fixture schema version");
    assert_eq!(
        fixture.k, MAX_SEARCH_LIMIT,
        "fixture k must follow the approved API contract"
    );

    let mut positive_queries = 0;
    let mut recalled = 0;
    let mut relevant_results = 0;
    let mut reciprocal_rank_sum = 0.0;
    let mut zero_results = 0;
    let mut refinements = 0;
    let mut recovered_refinements = 0;

    for case in &fixture.cases {
        let response = catalog
            .search(&case.query, Some(fixture.k), case.source.as_deref())
            .unwrap_or_else(|error| panic!("fixture case {} failed: {error}", case.id));
        assert_eq!(
            response.catalog_revision,
            catalog.revision(),
            "fixture case {} must report the loaded catalog revision",
            case.id
        );
        assert_eq!(
            response.results.is_empty(),
            case.expected_zero_results,
            "unexpected zero-result behavior for {}",
            case.id
        );
        for (index, result) in response.results.iter().enumerate() {
            assert_eq!(result.rank, index + 1, "stable rank for {}", case.id);
            assert!(!result.content_sha256.is_empty(), "version for {}", case.id);
        }

        if response.results.is_empty() {
            zero_results += 1;
        }
        if !case.acceptable_skill_ids.is_empty() {
            positive_queries += 1;
            let is_relevant = |result: &spur_core::explore::serving::SearchResult| {
                case.acceptable_skill_ids.contains(&result.name)
                    && case
                        .expected_source
                        .as_ref()
                        .is_none_or(|source| result.source == *source)
            };
            let first_relevant = response.results.iter().position(&is_relevant);
            if let Some(index) = first_relevant {
                recalled += 1;
                reciprocal_rank_sum += 1.0 / (index + 1) as f64;
            }
            relevant_results += response
                .results
                .iter()
                .filter(|result| is_relevant(result))
                .count();
        }

        if let Some(refinement) = &case.refinement {
            refinements += 1;
            assert!(
                response.results.is_empty(),
                "refinement case {} must begin with no result",
                case.id
            );
            let refined = catalog
                .search(&refinement.query, Some(fixture.k), case.source.as_deref())
                .unwrap_or_else(|error| panic!("refinement {} failed: {error}", case.id));
            if refined
                .results
                .iter()
                .any(|result| refinement.acceptable_skill_ids.contains(&result.name))
            {
                recovered_refinements += 1;
            }
        }
    }

    RetrievalMetrics {
        evaluated_queries: fixture.cases.len(),
        recall_at_5: fixed_rate(recalled, positive_queries),
        precision_at_5: fixed_rate(relevant_results, positive_queries * fixture.k),
        mrr: if positive_queries == 0 {
            "n/a".to_owned()
        } else {
            format!("{:.3}", reciprocal_rank_sum / positive_queries as f64)
        },
        zero_result_rate: fixed_rate(zero_results, fixture.cases.len()),
        refinement_recovery: fixed_rate(recovered_refinements, refinements),
    }
}

fn rooted_registry(repo_root: &Path) -> ToolRegistry {
    ToolRegistry::builder()
        .with(SkillsCatalogMcpModule::new(Some(repo_root)))
        .expect("register rooted skills catalog")
        .build()
}

fn tool_result_json(response: &spur_mcp::response::JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "unexpected MCP error: {:?}",
        response.error
    );
    let text = response
        .result
        .as_ref()
        .and_then(|result| result["content"][0]["text"].as_str())
        .expect("MCP JSON text result");
    serde_json::from_str(text).expect("parse MCP JSON text")
}

fn assert_error_kind(response: spur_mcp::response::JsonRpcResponse, expected: &str) {
    let error = response.error.expect("expected MCP error");
    assert_eq!(
        error.data,
        Some(json!({
            "error_kind": expected,
            "write_effect": "none"
        })),
        "MCP denial must use the stable error kind and remain write-free"
    );
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, current: &Path, output: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = fs::read_dir(current)
            .unwrap_or_else(|error| panic!("read {}: {error}", current.display()))
            .collect::<Result<Vec<_>, _>>()
            .expect("read directory entries");
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("relative path")
                .to_path_buf();
            let file_type = entry.file_type().expect("file type");
            if file_type.is_dir() {
                output.insert(relative.clone(), b"directory".to_vec());
                visit(root, &path, output);
            } else if file_type.is_symlink() {
                output.insert(
                    relative,
                    fs::read_link(&path)
                        .expect("symlink target")
                        .to_string_lossy()
                        .into_owned()
                        .into_bytes(),
                );
            } else {
                output.insert(relative, fs::read(&path).expect("file bytes"));
            }
        }
    }

    let mut output = BTreeMap::new();
    visit(root, root, &mut output);
    output
}

/// Unique PageIndex vocabulary used by SN5 integration coverage.
/// Frontmatter-only / section-only / resource-only tokens must not cross layers.
const SN5_FRONTMATTER_TOKEN: &str = "sn5frontmatteronlytoken";
const SN5_SECTION_TOKEN: &str = "sn5sectiononlytoken";
const SN5_RESOURCE_TOKEN: &str = "sn5resourceonlytoken";
const SN5_SCRIPT_TOKEN: &str = "sn5scriptonlytoken";
const SN5_BODY_SECRET: &str = "SN5_FULL_BODY_SECRET_NEVER_IN_NAVIGATE_HITS";
const SN5_SKILL_NAME: &str = "pageindex-nav";

fn populate_world(world: &TestWorld) -> PathBuf {
    let auth = world.bundled_skill(
        "auth-review",
        "Validate authentication changes before merging with a security review",
        "EXACT_AUTH_INSTRUCTIONS_NEVER_APPEAR_IN_SEARCH",
    );
    fs::create_dir_all(auth.join("references")).expect("create auth references");
    fs::write(
        auth.join("references/checklist.md"),
        "exact approved authentication checklist\n",
    )
    .expect("write auth checklist");
    world.bundled_skill(
        "deployment-rollback",
        "Deployment rollback and release recovery workflow",
        "rollback instructions",
    );

    // SN5 PageIndex three-layer fixture: frontmatter / section body / approved resource.
    // Description carries frontmatter-only vocabulary; body carries section-only
    // vocabulary that skill_search (name+description) must not surface.
    let pageindex = world.repo.path().join("assets/skills").join(SN5_SKILL_NAME);
    fs::create_dir_all(pageindex.join("references")).expect("create pageindex references");
    // Pad past LEDE_CHARS (200) so the full-body secret is only available via skill_read.
    let section_padding = "padding ".repeat(40);
    fs::write(
        pageindex.join("SKILL.md"),
        format!(
            "---\n\
             name: {SN5_SKILL_NAME}\n\
             description: Use when applying {SN5_FRONTMATTER_TOKEN} in catalog sessions\n\
             role: both\n\
             ---\n\
             \n\
             # PageIndex Navigation Fixture\n\
             \n\
             Section body carries {SN5_SECTION_TOKEN} for heading FTS and must never appear in name or description.\n\
             \n\
             {section_padding}\n\
             \n\
             {SN5_BODY_SECRET}\n"
        ),
    )
    .expect("write pageindex SKILL.md");
    fs::write(
        pageindex.join("references/sn5-guide.md"),
        format!(
            "# SN5 Resource Guide\n\nBody carries {SN5_RESOURCE_TOKEN} for approved resource FTS.\n"
        ),
    )
    .expect("write pageindex resource");

    for index in 0..6 {
        world.bundled_skill(
            &format!("bounded-candidate-{index}"),
            "Bounded catalog candidate metadata",
            &format!("SECRET_BOUNDED_BODY_{index}"),
        );
    }

    let script = world.bundled_skill("script-dependent", "script dependent workflow", "script");
    fs::create_dir_all(script.join("scripts")).expect("create scripts directory");
    fs::write(
        script.join("scripts/run.sh"),
        format!("#!/bin/sh\necho {SN5_SCRIPT_TOKEN}\n"),
    )
    .expect("write script");
    let binary = world.bundled_skill("binary-dependent", "binary dependent workflow", "binary");
    fs::write(binary.join("payload.png"), [0_u8, 159, 146, 150]).expect("write binary");
    let non_utf8 = world.bundled_skill("non-utf8", "non utf8 workflow", "non utf8");
    fs::write(non_utf8.join("notes.txt"), [0xff_u8, 0xfe]).expect("write non-UTF-8");
    let oversized = world.bundled_skill("oversized", "oversized workflow", "oversized");
    fs::write(
        oversized.join("large.md"),
        vec![b'x'; MAX_TEXT_CONTENT_BYTES + 1],
    )
    .expect("write oversized resource");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let linked = world.bundled_skill("symlinked", "symlinked workflow", "linked");
        symlink("SKILL.md", linked.join("guide.md")).expect("create symlink resource");
    }

    world.pool_skill(
        Layer::Global,
        PoolSkillSpec {
            name: "secure-review",
            source: "trusted/global",
            pinned_commit: &"a".repeat(40),
            description: "trusted global secure review",
            verdict: "clean",
            body: "TRUSTED_GLOBAL_REVIEW",
        },
    );
    world.pool_skill(
        Layer::Local,
        PoolSkillSpec {
            name: "secure-review",
            source: "untrusted/local",
            pinned_commit: &"b".repeat(40),
            description: "unapproved local secure review shadow",
            verdict: "flagged",
            body: "UNAPPROVED_LOCAL_SHADOW",
        },
    );
    world.pool_skill(
        Layer::Global,
        PoolSkillSpec {
            name: "supply-chain-audit",
            source: "trusted/catalog",
            pinned_commit: &"c".repeat(40),
            description: "Supply chain provenance audit workflow",
            verdict: "clean",
            body: "trusted supply chain instructions",
        },
    );
    world.pool_skill(
        Layer::Local,
        PoolSkillSpec {
            name: "revoked-incident-drill",
            source: "untrusted/local",
            pinned_commit: &"d".repeat(40),
            description: "Legacy incident drill workflow",
            verdict: "disabled",
            body: "revoked instructions",
        },
    );
    let integrity = world.pool_skill(
        Layer::Local,
        PoolSkillSpec {
            name: "integrity-check",
            source: "trusted/local",
            pinned_commit: &"e".repeat(40),
            description: "Integrity check workflow",
            verdict: "clean",
            body: "ORIGINAL_INTEGRITY_CONTENT",
        },
    );
    world.pool_skill(
        Layer::Local,
        PoolSkillSpec {
            name: "revocable-workflow",
            source: "trusted/local",
            pinned_commit: &"f".repeat(40),
            description: "Revocable workflow",
            verdict: "clean",
            body: "revocable instructions",
        },
    );
    world.pool_skill(
        Layer::Local,
        PoolSkillSpec {
            name: "concurrent-revision",
            source: "trusted/local",
            pinned_commit: &"1".repeat(40),
            description: "Concurrent revision workflow",
            verdict: "clean",
            body: "REVISION_ONE_CONTENT",
        },
    );
    integrity
}

#[tokio::test(flavor = "current_thread")]
async fn skills_catalog_rollout_gate_is_context_only_reversible_and_measurable() {
    let world = TestWorld::new();
    assert_eq!(store::global_root(), Some(world.global_root()));
    let integrity_dir = populate_world(&world);
    let auth_skill_bytes = fs::read(world.repo_root().join("assets/skills/auth-review/SKILL.md"))
        .expect("read exact auth skill fixture");

    let brain = rooted_registry(world.repo_root());
    let worker = rooted_registry(world.repo_root());
    for (registry_name, registry) in [("brain", &brain), ("worker", &worker)] {
        for tool_name in ["skill_search", "skill_read", "skill_navigate"] {
            assert!(
                registry
                    .list_tools()
                    .iter()
                    .any(|tool| tool.name == tool_name),
                "rooted {registry_name} registry missing {tool_name}"
            );
        }
    }

    let before_reads = snapshot_tree(world.repo_root());
    let brain_search = brain
        .call_json_tool(
            ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None),
            "skill_search",
            json!({ "query": "auth review" }),
        )
        .await;
    let brain_search = tool_result_json(&brain_search);
    assert!(brain_search.get("content").is_none());
    assert!(!brain_search
        .to_string()
        .contains("EXACT_AUTH_INSTRUCTIONS_NEVER_APPEAR_IN_SEARCH"));
    assert!(brain_search["results"].as_array().is_some_and(|results| {
        !results.is_empty()
            && results.len() <= MAX_SEARCH_LIMIT
            && results.iter().all(|result| result.get("content").is_none())
    }));
    let auth_skill_id = brain_search["results"][0]["skill_id"]
        .as_str()
        .expect("auth opaque reference")
        .to_owned();

    let brain_read = brain
        .call_json_tool(
            ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None),
            "skill_read",
            json!({ "skill_id": auth_skill_id }),
        )
        .await;
    let brain_read = tool_result_json(&brain_read);
    assert_eq!(brain_read["resource"], "SKILL.md");
    assert_eq!(
        brain_read["content"].as_str().map(str::as_bytes),
        Some(auth_skill_bytes.as_slice()),
        "skill_read must return the exact SKILL.md fixture bytes"
    );

    let worker_search = worker
        .call_json_tool(
            ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None),
            "skill_search",
            json!({ "query": "auth review" }),
        )
        .await;
    let worker_search = tool_result_json(&worker_search);
    assert_eq!(
        worker_search["results"][0]["skill_id"],
        brain_search["results"][0]["skill_id"]
    );
    let worker_read = worker
        .call_json_tool(
            ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None),
            "skill_read",
            json!({
                "skill_id": auth_skill_id,
                "resource": "references/checklist.md"
            }),
        )
        .await;
    let worker_read = tool_result_json(&worker_read);
    assert_eq!(
        worker_read["content"],
        "exact approved authentication checklist\n"
    );

    let bounded = worker
        .call_json_tool(
            ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None),
            "skill_search",
            json!({ "query": "bounded catalog candidate" }),
        )
        .await;
    let bounded = tool_result_json(&bounded);
    assert_eq!(bounded["results"].as_array().map(Vec::len), Some(5));
    assert!(!bounded.to_string().contains("SECRET_BOUNDED_BODY"));

    for resource in [
        "/etc/passwd",
        "../deployment-rollback/SKILL.md",
        "references/../SKILL.md",
        "scripts/run.sh",
        "references/payload.png",
    ] {
        let response = worker
            .call_json_tool(
                ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None),
                "skill_read",
                json!({ "skill_id": auth_skill_id, "resource": resource }),
            )
            .await;
        assert_error_kind(response, "resource_denied");
    }
    let undeclared = worker
        .call_json_tool(
            ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None),
            "skill_read",
            json!({
                "skill_id": auth_skill_id,
                "resource": "references/not-declared.md"
            }),
        )
        .await;
    assert_error_kind(undeclared, "resource_not_found");
    assert_eq!(snapshot_tree(world.repo_root()), before_reads);

    let catalog = ServingCatalog::load(world.repo_root()).expect("load serving catalog");
    for (name, compatibility) in [
        ("script-dependent", ContextCompatibility::RequiresScripts),
        ("binary-dependent", ContextCompatibility::BinaryResources),
        ("non-utf8", ContextCompatibility::NonUtf8Text),
        ("oversized", ContextCompatibility::ContentTooLarge),
    ] {
        assert_eq!(
            catalog
                .decision(name, "bundled")
                .expect("compatibility decision")
                .compatibility,
            compatibility
        );
        assert!(catalog
            .search(name, None, None)
            .expect("search incompatible skill")
            .results
            .is_empty());
    }
    #[cfg(unix)]
    {
        assert_eq!(
            catalog
                .decision("symlinked", "bundled")
                .expect("symlink decision")
                .compatibility,
            ContextCompatibility::SymlinkResource
        );
        assert!(catalog
            .search("symlinked", None, None)
            .expect("search symlinked skill")
            .results
            .is_empty());
    }

    let shadow = catalog
        .search("secure review", None, None)
        .expect("search shadowed skill");
    let secure_review_hits = shadow
        .results
        .iter()
        .filter(|result| result.name == "secure-review")
        .collect::<Vec<_>>();
    assert_eq!(secure_review_hits.len(), 1);
    assert_eq!(secure_review_hits[0].source, "trusted/global");
    assert!(!shadow
        .results
        .iter()
        .any(|result| result.source == "untrusted/local"));
    assert!(!secure_review_hits[0]
        .description
        .contains("unapproved local"));

    let integrity_catalog = ServingCatalog::load(world.repo_root()).expect("integrity catalog");
    let integrity_hit = integrity_catalog
        .search("integrity check", None, None)
        .expect("integrity search")
        .results
        .into_iter()
        .next()
        .expect("integrity hit");
    fs::write(
        integrity_dir.join("SKILL.md"),
        "---\nname: integrity-check\ndescription: Integrity check workflow\n---\nTAMPERED\n",
    )
    .expect("tamper with pinned content");
    assert_eq!(
        integrity_catalog
            .read(&integrity_hit.skill_id, None)
            .expect_err("integrity mismatch")
            .kind(),
        ServingErrorKind::IntegrityMismatch
    );

    let revocable_catalog = ServingCatalog::load(world.repo_root()).expect("revocable catalog");
    let revocable_hit = revocable_catalog
        .search("revocable", None, None)
        .expect("revocable search")
        .results
        .into_iter()
        .next()
        .expect("revocable hit");
    world.set_verdict(Layer::Local, "revocable-workflow", "disabled");
    assert_eq!(
        revocable_catalog
            .read(&revocable_hit.skill_id, None)
            .expect_err("revoked read")
            .kind(),
        ServingErrorKind::SkillNotEligible
    );

    let revision_one = ServingCatalog::load(world.repo_root()).expect("revision one catalog");
    let revision_one_hit = revision_one
        .search("concurrent revision", None, None)
        .expect("revision one search")
        .results
        .into_iter()
        .next()
        .expect("revision one hit");
    world.pool_skill(
        Layer::Local,
        PoolSkillSpec {
            name: "concurrent-revision",
            source: "trusted/local",
            pinned_commit: &"2".repeat(40),
            description: "Concurrent revision workflow",
            verdict: "clean",
            body: "REVISION_TWO_CONTENT",
        },
    );
    let revision_two = ServingCatalog::load(world.repo_root()).expect("revision two catalog");
    let revision_two_hit = revision_two
        .search("concurrent revision", None, None)
        .expect("revision two search")
        .results
        .into_iter()
        .next()
        .expect("revision two hit");
    assert_ne!(revision_one.revision(), revision_two.revision());
    let (old_read, current_read) = std::thread::scope(|scope| {
        let old = scope.spawn(|| revision_one.read(&revision_one_hit.skill_id, None));
        let current = scope.spawn(|| revision_two.read(&revision_two_hit.skill_id, None));
        (
            old.join().expect("old revision reader"),
            current.join().expect("current revision reader"),
        )
    });
    assert_eq!(
        old_read.expect_err("stale reference").kind(),
        ServingErrorKind::StaleSkillRef
    );
    let current_read = current_read.expect("current revision read");
    assert!(current_read.content.contains("REVISION_TWO_CONTENT"));
    assert!(!current_read.content.contains("REVISION_ONE_CONTENT"));
    assert_eq!(current_read.content_sha256, revision_two_hit.content_sha256);

    let fixture: RetrievalFixture =
        serde_json::from_str(RETRIEVAL_FIXTURE).expect("parse retrieval fixture");
    let evaluation_catalog =
        ServingCatalog::load(world.repo_root()).expect("evaluation serving catalog");
    let metrics = evaluate_retrieval(&evaluation_catalog, &fixture);
    let repeated_metrics = evaluate_retrieval(&evaluation_catalog, &fixture);
    assert_eq!(
        metrics, repeated_metrics,
        "retrieval evaluation is deterministic"
    );
    eprintln!(
        "skills_catalog_metrics {}",
        serde_json::to_string(&metrics).expect("serialize retrieval metrics")
    );

    let projection_repo = tempfile::tempdir().expect("projection repository");
    let bootstrap_dir = projection_repo.path().join("assets/skills/skills-catalog");
    fs::create_dir_all(&bootstrap_dir).expect("create projection bootstrap");
    fs::write(bootstrap_dir.join("SKILL.md"), BOOTSTRAP_SKILL).expect("write projection bootstrap");
    let other_dir = projection_repo.path().join("assets/skills/other-bundled");
    fs::create_dir_all(&other_dir).expect("create other bundled skill");
    fs::write(
        other_dir.join("SKILL.md"),
        "---\nname: other-bundled\ndescription: other workflow\nrole: both\n---\nother\n",
    )
    .expect("write other bundled skill");
    let foundation = resolve_effective_skills(
        projection_repo.path(),
        Adapter::Codex,
        RuntimeRole::Worker,
        SelectionPolicy::CatalogOnly,
    )
    .expect("foundation/catalog-only projection");
    assert!(foundation
        .iter()
        .any(|skill| skill.payload.id == "skills-catalog"));
    assert!(
        foundation
            .iter()
            .all(|skill| skill.payload.id != "other-bundled"),
        "non-foundation bundled skills must not materialize under catalog_only"
    );

    assert_eq!(
        SpurConfig::default().skills.projection_mode,
        SkillsProjectionMode::CatalogOnly
    );
    let configured: SpurConfig = toml::from_str("[skills]\nprojection_mode = \"all_active\"\n")
        .expect("parse all_active rollback");
    assert_eq!(
        configured.skills.projection_mode,
        SkillsProjectionMode::AllActive
    );
    let rollback = resolve_effective_skills(
        projection_repo.path(),
        Adapter::Codex,
        RuntimeRole::Worker,
        SelectionPolicy::AllActive,
    )
    .expect("all-active rollback projection");
    assert!(rollback
        .iter()
        .any(|skill| skill.payload.id == "skills-catalog"));
    assert!(rollback
        .iter()
        .any(|skill| skill.payload.id == "other-bundled"));
    assert!(rollback.len() > foundation.len());

    let unavailable = ToolRegistry::builder()
        .with(SkillsCatalogMcpModule::new(None))
        .expect("register unrooted catalog")
        .build();
    let before_unavailable = snapshot_tree(world.repo_root());
    let unavailable = unavailable
        .call_json_tool(
            ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None),
            "skill_search",
            json!({ "query": "specialized workflow" }),
        )
        .await;
    assert_error_kind(unavailable, "authority_root_required");
    assert_eq!(snapshot_tree(world.repo_root()), before_unavailable);
    for required_protocol in [
        "continue with the base agent's capabilities",
        "Do not search the filesystem for catalog content",
        "do not ask to install or materialize a task-specific skill",
    ] {
        assert!(
            BOOTSTRAP_SKILL.contains(required_protocol),
            "bootstrap fallback missing: {required_protocol}"
        );
    }
}

#[derive(Debug, Deserialize)]
struct NavigateFixture {
    schema_version: u32,
    skill_name: String,
    cases: Vec<NavigateCase>,
}

#[derive(Debug, Deserialize)]
struct NavigateCase {
    id: String,
    query: String,
    layer: String,
    #[serde(default)]
    expected_name: Option<String>,
    #[serde(default)]
    expected_node_kind: Option<String>,
    #[serde(default)]
    expected_path: Option<String>,
    #[serde(default)]
    expected_heading: Option<String>,
    skill_search_must_hit: bool,
    navigate_must_hit: bool,
}

fn assert_navigate_hit_is_metadata_only(hit: &Value, case_id: &str) {
    assert!(
        hit.get("content").is_none(),
        "navigate hit for {case_id} must not embed full content"
    );
    assert!(
        hit.get("skill_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "navigate hit for {case_id} needs skill_id"
    );
    assert!(
        hit.get("node_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "navigate hit for {case_id} needs node_id"
    );
    if let Some(lede) = hit.get("lede").and_then(Value::as_str) {
        assert!(
            lede.chars().count() <= 200,
            "navigate lede for {case_id} must stay within LEDE_CHARS"
        );
        assert!(
            !lede.contains(SN5_BODY_SECRET),
            "navigate lede for {case_id} must not leak the full-body secret"
        );
        assert!(
            !lede.contains(SN5_SCRIPT_TOKEN),
            "navigate lede for {case_id} must never surface script vocabulary"
        );
    }
}

/// SN5: skill_navigate PageIndex three-layer index works end-to-end over MCP.
///
/// Covers: (1) FTS via frontmatter-only vocabulary, (2) FTS via section body not
/// in name/description, (3) FTS via approved resource body, (4) tree hop lists
/// SKILL.md + resources and excludes scripts, (5) navigate then skill_read stays
/// write_effect none and reauth-clean, (6) brain and worker both advertise
/// skill_navigate.
#[tokio::test(flavor = "current_thread")]
async fn skill_navigate_pageindex_three_layer_index_works_end_to_end() {
    let world = TestWorld::new();
    assert_eq!(store::global_root(), Some(world.global_root()));
    let _integrity_dir = populate_world(&world);

    let fixture: NavigateFixture =
        serde_json::from_str(NAVIGATE_FIXTURE).expect("parse navigate fixture");
    assert_eq!(fixture.schema_version, 1, "navigate fixture schema");
    assert_eq!(fixture.skill_name, SN5_SKILL_NAME);

    let brain = rooted_registry(world.repo_root());
    let worker = rooted_registry(world.repo_root());

    // (6) brain and worker both advertise skill_navigate.
    for (registry_name, registry) in [("brain", &brain), ("worker", &worker)] {
        let tools = registry.list_tools();
        assert!(
            tools.iter().any(|tool| tool.name == "skill_navigate"),
            "{registry_name} must advertise skill_navigate"
        );
        assert!(
            tools.iter().any(|tool| tool.name == "skill_search"),
            "{registry_name} must advertise skill_search"
        );
        assert!(
            tools.iter().any(|tool| tool.name == "skill_read"),
            "{registry_name} must advertise skill_read"
        );
    }

    let before = snapshot_tree(world.repo_root());
    let worker_ctx = || ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None);
    let brain_ctx = || ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None);

    // (1)(2)(3)(+ scripts never indexed): fixture-driven FTS cases.
    let mut pageindex_skill_id = None;
    for case in &fixture.cases {
        let search = worker
            .call_json_tool(worker_ctx(), "skill_search", json!({ "query": case.query }))
            .await;
        let search = tool_result_json(&search);
        let search_hit = search["results"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|result| result["name"] == SN5_SKILL_NAME);
        assert_eq!(
            search_hit, case.skill_search_must_hit,
            "skill_search hit mismatch for {} ({})",
            case.id, case.layer
        );
        assert!(
            search.get("content").is_none(),
            "skill_search for {} must not return bodies",
            case.id
        );

        let navigate = worker
            .call_json_tool(
                worker_ctx(),
                "skill_navigate",
                json!({ "query": case.query }),
            )
            .await;
        let navigate = tool_result_json(&navigate);
        assert!(
            navigate.get("content").is_none(),
            "skill_navigate for {} must not return full bodies",
            case.id
        );
        assert!(navigate["catalog_revision"].as_str().is_some());
        let hits = navigate["hits"]
            .as_array()
            .expect("hits array")
            .iter()
            .collect::<Vec<_>>();
        assert!(
            hits.len() <= MAX_SEARCH_LIMIT,
            "navigate limit for {}",
            case.id
        );

        let matching: Vec<&Value> = hits
            .iter()
            .copied()
            .filter(|hit| {
                case.expected_name
                    .as_ref()
                    .is_none_or(|name| hit["name"].as_str() == Some(name.as_str()))
                    && case
                        .expected_node_kind
                        .as_ref()
                        .is_none_or(|kind| hit["node_kind"].as_str() == Some(kind.as_str()))
                    && case
                        .expected_path
                        .as_ref()
                        .is_none_or(|path| hit["path"].as_str() == Some(path.as_str()))
                    && case
                        .expected_heading
                        .as_ref()
                        .is_none_or(|heading| hit["heading"].as_str() == Some(heading.as_str()))
                    && hit["lede"]
                        .as_str()
                        .is_some_and(|lede| lede.contains(&case.query))
            })
            .collect();

        if case.navigate_must_hit {
            assert!(
                !matching.is_empty(),
                "navigate must hit layer {} for {}: {:?}",
                case.layer,
                case.id,
                hits
            );
            for hit in &matching {
                assert_navigate_hit_is_metadata_only(hit, &case.id);
                assert!(hit["score"].as_f64().is_some(), "FTS hit needs score");
            }
            if pageindex_skill_id.is_none() {
                pageindex_skill_id = matching[0]["skill_id"].as_str().map(str::to_owned);
            }
        } else {
            assert!(
                matching.is_empty()
                    && hits.iter().all(|hit| {
                        hit["lede"]
                            .as_str()
                            .map(|lede| !lede.contains(&case.query))
                            .unwrap_or(true)
                            && hit["name"].as_str() != Some("script-dependent")
                    }),
                "navigate must not index scripts for {}: {:?}",
                case.id,
                hits
            );
        }
    }

    let pageindex_skill_id = pageindex_skill_id.expect("pageindex skill_id from FTS hits");

    // (4) tree hop lists SKILL.md + approved resources and excludes scripts.
    let root = worker
        .call_json_tool(
            worker_ctx(),
            "skill_navigate",
            json!({ "root": pageindex_skill_id, "limit": 5 }),
        )
        .await;
    let root = tool_result_json(&root);
    let root_hits = root["hits"].as_array().expect("tree hop hits");
    assert!(!root_hits.is_empty(), "tree hop must return children");
    let paths: Vec<&str> = root_hits
        .iter()
        .filter_map(|hit| hit["path"].as_str())
        .collect();
    let kinds: Vec<&str> = root_hits
        .iter()
        .filter_map(|hit| hit["node_kind"].as_str())
        .collect();
    assert!(
        kinds.iter().any(|kind| *kind == "frontmatter")
            && paths.iter().any(|path| *path == "SKILL.md"),
        "tree hop must list SKILL.md frontmatter: kinds={kinds:?} paths={paths:?}"
    );
    assert!(
        kinds.iter().any(|kind| *kind == "document")
            && paths.iter().any(|path| *path == "SKILL.md"),
        "tree hop must list SKILL.md document: kinds={kinds:?} paths={paths:?}"
    );
    assert!(
        kinds.iter().any(|kind| *kind == "resource")
            && paths.iter().any(|path| *path == "references/sn5-guide.md"),
        "tree hop must list approved resources: kinds={kinds:?} paths={paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.starts_with("scripts/")),
        "tree hop must exclude scripts paths: {paths:?}"
    );
    assert!(
        !kinds.iter().any(|kind| *kind == "section"),
        "skill root hop is one level — no section dump: {kinds:?}"
    );
    for hit in root_hits {
        assert_navigate_hit_is_metadata_only(hit, "tree-hop");
        assert!(
            hit.get("score").is_none() || hit["score"].is_null(),
            "tree hop hits have no FTS score"
        );
    }

    // (5) navigate then skill_read remains write_effect none and reauth-clean.
    let brain_navigate = brain
        .call_json_tool(
            brain_ctx(),
            "skill_navigate",
            json!({ "query": SN5_SECTION_TOKEN }),
        )
        .await;
    let brain_navigate = tool_result_json(&brain_navigate);
    let brain_skill_id = brain_navigate["hits"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|hit| hit["name"] == SN5_SKILL_NAME)
        .and_then(|hit| hit["skill_id"].as_str())
        .expect("brain navigate hit")
        .to_owned();
    assert_eq!(brain_skill_id, pageindex_skill_id);

    let worker_read = worker
        .call_json_tool(
            worker_ctx(),
            "skill_read",
            json!({ "skill_id": pageindex_skill_id }),
        )
        .await;
    let worker_read = tool_result_json(&worker_read);
    assert_eq!(worker_read["resource"], "SKILL.md");
    let content = worker_read["content"].as_str().expect("skill_read content");
    assert!(
        content.contains(SN5_SECTION_TOKEN) && content.contains(SN5_BODY_SECRET),
        "reauth skill_read must return exact SKILL.md body"
    );
    assert!(
        content.contains(SN5_FRONTMATTER_TOKEN),
        "SKILL.md frontmatter remains available only via exact read"
    );

    let resource_read = worker
        .call_json_tool(
            worker_ctx(),
            "skill_read",
            json!({
                "skill_id": pageindex_skill_id,
                "resource": "references/sn5-guide.md"
            }),
        )
        .await;
    let resource_read = tool_result_json(&resource_read);
    assert_eq!(resource_read["resource"], "references/sn5-guide.md");
    assert!(resource_read["content"]
        .as_str()
        .is_some_and(|text| text.contains(SN5_RESOURCE_TOKEN)));

    // Denied paths stay write-free after navigate handoff.
    let denied = worker
        .call_json_tool(
            worker_ctx(),
            "skill_read",
            json!({
                "skill_id": pageindex_skill_id,
                "resource": "scripts/run.sh"
            }),
        )
        .await;
    assert_error_kind(denied, "resource_denied");

    let missing_root = worker
        .call_json_tool(
            worker_ctx(),
            "skill_navigate",
            json!({ "root": "skillref.v1.deadbeef.deadbeef" }),
        )
        .await;
    assert_error_kind(missing_root, "skill_not_found");

    assert_eq!(
        snapshot_tree(world.repo_root()),
        before,
        "navigate + read must leave the repository tree unchanged (write_effect none)"
    );
}
