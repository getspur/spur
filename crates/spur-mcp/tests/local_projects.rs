use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex, OnceLock};

use serde_json::json;
use spur_mcp::local_projects::{
    decorate_project_response, extract_project, with_optional_project_schema, LocalProjectAccess,
    LocalProjectCatalogMcpModule, LocalProjectCatalogStore, LocalProjectError, LocalProjectHealth,
    LocalProjectResolver, LocalProjectStatus, LocalProjectValidator, ValidatedLocalProject,
};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolModule as _, ToolRegistry};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Default)]
struct FakeValidator {
    unavailable: Arc<Mutex<Vec<PathBuf>>>,
}

impl FakeValidator {
    fn unavailable(&self, root: impl Into<PathBuf>) {
        self.unavailable
            .lock()
            .expect("validator lock")
            .push(root.into());
    }
}

impl LocalProjectValidator for FakeValidator {
    fn validate(&self, requested_path: &Path) -> Result<ValidatedLocalProject, LocalProjectError> {
        let canonical_root =
            requested_path
                .canonicalize()
                .map_err(|error| LocalProjectError::InvalidPath {
                    path: requested_path.to_path_buf(),
                    reason: error.to_string(),
                })?;
        if self
            .unavailable
            .lock()
            .expect("validator lock")
            .contains(&canonical_root)
        {
            return Ok(ValidatedLocalProject {
                canonical_root,
                health: LocalProjectHealth::unavailable("analyst index is missing"),
            });
        }
        Ok(ValidatedLocalProject {
            canonical_root,
            health: LocalProjectHealth::ready(),
        })
    }
}

fn store(temp: &tempfile::TempDir) -> LocalProjectCatalogStore {
    LocalProjectCatalogStore::new(temp.path().join("config/projects.toml"))
}

fn validator() -> Arc<dyn LocalProjectValidator> {
    Arc::new(FakeValidator::default())
}

fn make_root(temp: &tempfile::TempDir, name: &str) -> PathBuf {
    let root = temp.path().join(name);
    std::fs::create_dir_all(&root).expect("create fixture root");
    root
}

#[test]
fn explicit_and_environment_catalog_paths_follow_precedence() {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("environment lock");
    let temp = tempfile::tempdir().expect("tempdir");
    let explicit = temp.path().join("explicit.toml");
    let xdg = temp.path().join("xdg");
    let home = temp.path().join("home");
    std::env::set_var("SPUR_PROJECT_CATALOG", &explicit);
    std::env::set_var("XDG_CONFIG_HOME", &xdg);
    std::env::set_var("HOME", &home);
    assert_eq!(
        LocalProjectCatalogStore::from_environment()
            .catalog_path()
            .expect("explicit catalog path"),
        explicit
    );
    std::env::remove_var("SPUR_PROJECT_CATALOG");
    assert_eq!(
        LocalProjectCatalogStore::from_environment()
            .catalog_path()
            .expect("xdg catalog path"),
        xdg.join("spur/projects.toml")
    );
    std::env::remove_var("XDG_CONFIG_HOME");
    assert_eq!(
        LocalProjectCatalogStore::from_environment()
            .catalog_path()
            .expect("home catalog path"),
        home.join(".config/spur/projects.toml")
    );
    std::env::remove_var("HOME");
}

#[test]
fn relative_or_missing_config_roots_fail_lazily() {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("environment lock");
    let store = LocalProjectCatalogStore::from_environment();

    std::env::remove_var("SPUR_PROJECT_CATALOG");
    std::env::set_var("XDG_CONFIG_HOME", "relative-xdg");
    std::env::set_var("HOME", "/absolute/home");
    assert!(matches!(
        store.catalog_path(),
        Err(LocalProjectError::ConfigUnavailable { .. })
    ));

    std::env::remove_var("XDG_CONFIG_HOME");
    std::env::set_var("HOME", "relative-home");
    assert!(matches!(
        store.catalog_path(),
        Err(LocalProjectError::ConfigUnavailable { .. })
    ));

    std::env::remove_var("HOME");
    assert!(matches!(
        store.catalog_path(),
        Err(LocalProjectError::ConfigUnavailable { .. })
    ));
}

#[test]
fn catalog_mutations_are_versioned_sorted_and_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let alpha = make_root(&temp, "alpha");
    let beta = make_root(&temp, "beta");
    let replacement = make_root(&temp, "replacement");
    let store = store(&temp);

    let empty = store.snapshot().expect("fresh catalog");
    assert_eq!(empty.version, 1);
    assert_eq!(empty.generation, 0);
    assert!(empty.projects.is_empty());

    let first = store.add("beta", &beta, false).expect("add beta");
    assert!(first.changed);
    assert_eq!(first.catalog_generation, 1);
    let duplicate = store.add("beta", &beta, false).expect("idempotent add");
    assert!(!duplicate.changed);
    assert_eq!(duplicate.catalog_generation, 1);
    store.add("alpha", &alpha, false).expect("add alpha");

    let conflict = store
        .add("alpha", &replacement, false)
        .expect_err("conflict");
    assert!(matches!(conflict, LocalProjectError::Conflict { .. }));
    let replaced = store
        .add("alpha", &replacement, true)
        .expect("replace alpha");
    assert!(replaced.changed);
    assert_eq!(replaced.catalog_generation, 3);

    let snapshot = store.snapshot().expect("snapshot");
    assert_eq!(
        snapshot
            .projects
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
    let text = std::fs::read_to_string(store.catalog_path().expect("path")).expect("catalog");
    assert!(text.starts_with("version = 1\ngeneration = 3"));

    let removed = store.remove("beta").expect("remove beta");
    assert!(removed.removed);
    assert_eq!(removed.catalog_generation, 4);
    let missing = store.remove("beta").expect("idempotent remove");
    assert!(!missing.removed);
    assert_eq!(missing.catalog_generation, 4);
}

#[test]
fn catalog_rejects_bad_names_paths_and_versions() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = make_root(&temp, "root");
    let store = store(&temp);
    for name in ["", "-bad", "has space", &"x".repeat(65)] {
        assert!(matches!(
            store.add(name, &root, false),
            Err(LocalProjectError::InvalidName { .. })
        ));
    }
    assert!(matches!(
        store.add("relative", Path::new("relative"), false),
        Err(LocalProjectError::InvalidPath { .. })
    ));
    assert!(matches!(
        store.add("missing", &temp.path().join("missing"), false),
        Err(LocalProjectError::InvalidPath { .. })
    ));

    let path = store.catalog_path().expect("path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, "version = 2\ngeneration = 0\nprojects = []\n").expect("write");
    assert!(matches!(
        store.snapshot(),
        Err(LocalProjectError::UnsupportedVersion { version: 2, .. })
    ));

    std::fs::write(&path, "not = [valid toml").expect("write corrupt catalog");
    assert!(matches!(
        store.snapshot(),
        Err(LocalProjectError::CatalogParse { .. })
    ));

    let root = root.display();
    std::fs::write(
        &path,
        format!(
            "version = 1\ngeneration = 2\n\n[[projects]]\nname = \"dup\"\nroot = \"{root}\"\n\n[[projects]]\nname = \"dup\"\nroot = \"{root}\"\n"
        ),
    )
    .expect("write duplicate catalog");
    assert!(matches!(
        store.snapshot(),
        Err(LocalProjectError::DuplicateName { .. })
    ));
}

#[cfg(unix)]
#[test]
fn catalog_rejects_non_utf8_paths_and_uses_private_permissions() {
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().expect("tempdir");
    let store = store(&temp);
    let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]));
    assert!(matches!(
        store.add("invalid", &invalid, false),
        Err(LocalProjectError::InvalidPath { .. })
    ));
    let non_utf8_target = temp.path().join(std::ffi::OsString::from_vec(vec![
        b'r', b'o', b'o', b't', 0xff,
    ]));
    std::fs::create_dir(&non_utf8_target).expect("create non-UTF-8 target");
    let utf8_link = temp.path().join("utf8-link");
    symlink(&non_utf8_target, &utf8_link).expect("create UTF-8 symlink");
    assert!(matches!(
        store.add("symlink", &utf8_link, false),
        Err(LocalProjectError::InvalidPath { .. })
    ));
    let root = make_root(&temp, "root");
    store.add("root", &root, false).expect("add root");
    let path = store.catalog_path().expect("path");
    assert_eq!(
        std::fs::metadata(path.parent().expect("parent"))
            .expect("dir metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for file in [&path, &path.with_extension("toml.lock")] {
        assert_eq!(
            std::fs::metadata(file)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn catalog_symlinks_fail_closed_without_replacing_directory_entries() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("config");
    std::fs::create_dir(&parent).expect("create catalog parent");
    let root = make_root(&temp, "root");

    for operation in ["snapshot", "add", "remove"] {
        let path = parent.join(format!("{operation}.toml"));
        let target = parent.join(format!("missing-{operation}.toml"));
        symlink(&target, &path).expect("create dangling catalog symlink");
        let store = LocalProjectCatalogStore::new(path.clone());

        let error = match operation {
            "snapshot" => store.snapshot().map(|_| ()),
            "add" => store.add("alpha", &root, false).map(|_| ()),
            "remove" => store.remove("alpha").map(|_| ()),
            _ => unreachable!(),
        }
        .expect_err("catalog symlink must fail closed");

        assert!(
            matches!(
                error,
                LocalProjectError::CatalogRead { .. } | LocalProjectError::CatalogWrite { .. }
            ),
            "unexpected error: {error}"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("symlink metadata")
                .file_type()
                .is_symlink(),
            "catalog path was replaced during {operation}"
        );
        assert_eq!(std::fs::read_link(&path).expect("read symlink"), target);
        assert!(
            !target.exists(),
            "dangling target was created by {operation}"
        );
    }

    let target = parent.join("real-catalog.toml");
    std::fs::write(&target, "version = 1\ngeneration = 0\nprojects = []\n")
        .expect("write target catalog");
    let target_contents = std::fs::read(&target).expect("read target bytes");
    let path = parent.join("linked-catalog.toml");
    symlink(&target, &path).expect("create catalog symlink");
    let error = LocalProjectCatalogStore::new(path.clone())
        .snapshot()
        .expect_err("catalog symlink to a regular file must be rejected");
    assert!(matches!(error, LocalProjectError::CatalogRead { .. }));
    assert_eq!(std::fs::read_link(&path).expect("read symlink"), target);
    assert_eq!(
        std::fs::read(&target).expect("read unchanged target"),
        target_contents
    );
}

#[cfg(unix)]
#[test]
fn lock_file_symlinks_fail_closed_without_touching_targets() {
    use std::os::unix::fs::{symlink, PermissionsExt as _};

    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("config");
    std::fs::create_dir(&parent).expect("create catalog parent");
    let root = make_root(&temp, "root");

    for operation in ["snapshot", "add", "remove"] {
        let path = parent.join(format!("{operation}.toml"));
        let lock_path = path.with_extension("toml.lock");
        let target = parent.join(format!("lock-target-{operation}"));
        let target_bytes = format!("sentinel-{operation}").into_bytes();
        std::fs::write(&target, &target_bytes).expect("write lock target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
            .expect("set lock target mode");
        symlink(&target, &lock_path).expect("create lock symlink");
        let store = LocalProjectCatalogStore::new(path.clone());

        let error = match operation {
            "snapshot" => store.snapshot().map(|_| ()),
            "add" => store.add("alpha", &root, false).map(|_| ()),
            "remove" => store.remove("alpha").map(|_| ()),
            _ => unreachable!(),
        }
        .expect_err("lock symlink must fail closed");

        assert!(matches!(error, LocalProjectError::CatalogWrite { .. }));
        assert!(
            std::fs::symlink_metadata(&lock_path)
                .expect("lock symlink metadata")
                .file_type()
                .is_symlink(),
            "lock path was replaced during {operation}"
        );
        assert_eq!(
            std::fs::read_link(&lock_path).expect("read lock link"),
            target
        );
        assert_eq!(
            std::fs::read(&target).expect("read lock target"),
            target_bytes,
            "lock target bytes changed during {operation}"
        );
        assert_eq!(
            std::fs::metadata(&target)
                .expect("lock target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o644,
            "lock target mode changed during {operation}"
        );
        assert!(
            !path.exists(),
            "catalog mutation proceeded after unsafe lock during {operation}"
        );
    }
}

#[cfg(unix)]
#[test]
fn existing_explicit_parent_permissions_are_preserved() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("explicit-parent");
    std::fs::create_dir(&parent).expect("create explicit parent");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
        .expect("set explicit parent permissions");
    let store = LocalProjectCatalogStore::new(parent.join("projects.toml"));

    store.snapshot().expect("snapshot fresh catalog");
    assert_eq!(
        std::fs::metadata(&parent)
            .expect("parent metadata after snapshot")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );

    let root = make_root(&temp, "root");
    store.add("root", &root, false).expect("add root");
    assert_eq!(
        std::fs::metadata(&parent)
            .expect("parent metadata after add")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn existing_catalog_permissions_are_repaired_on_reads_and_noop_mutations() {
    use std::os::unix::fs::PermissionsExt as _;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let root = make_root(&temp, "root");
    let store = store(&temp);
    store.add("root", &root, false).expect("seed catalog");
    let path = store.catalog_path().expect("catalog path");
    let lock = path.with_extension("toml.lock");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("loosen catalog before read");
    store.snapshot().expect("read repairs catalog mode");
    assert_eq!(mode(&path), 0o600);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("loosen catalog before idempotent add");
    let add = store.add("root", &root, false).expect("idempotent add");
    assert!(!add.changed);
    assert_eq!(mode(&path), 0o600);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("loosen catalog before idempotent remove");
    let remove = store.remove("missing").expect("idempotent remove");
    assert!(!remove.removed);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&lock), 0o600);
}

#[test]
fn concurrent_store_instances_do_not_lose_updates() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store_a = store(&temp);
    let store_b = store(&temp);
    let alpha = make_root(&temp, "alpha");
    let beta = make_root(&temp, "beta");
    let barrier = Arc::new(Barrier::new(3));
    let first = {
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            store_a.add("alpha", &alpha, false).expect("add alpha");
        })
    };
    let second = {
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            store_b.add("beta", &beta, false).expect("add beta");
        })
    };
    barrier.wait();
    first.join().expect("first thread");
    second.join().expect("second thread");
    let snapshot = store(&temp).snapshot().expect("snapshot");
    assert_eq!(snapshot.generation, 2);
    assert_eq!(snapshot.projects.len(), 2);
}

#[test]
fn generation_overflow_fails_without_reporting_or_persisting_a_change() {
    let temp = tempfile::tempdir().expect("tempdir");
    let alpha = make_root(&temp, "alpha");
    let beta = make_root(&temp, "beta");
    let store = store(&temp);
    let path = store.catalog_path().expect("path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let previous = format!(
        "version = 1\ngeneration = {}\n\n[[projects]]\nname = \"alpha\"\nroot = \"{}\"\n",
        i64::MAX,
        alpha.display()
    );
    std::fs::write(&path, &previous).expect("write maximum-generation catalog");

    let result = store.add("beta", &beta, false);

    assert!(matches!(result, Err(LocalProjectError::GenerationOverflow)));
    assert_eq!(std::fs::read_to_string(path).expect("catalog"), previous);
}

#[test]
fn resolver_reports_live_health_without_pruning_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = make_root(&temp, "root");
    let fake = FakeValidator::default();
    let validator: Arc<dyn LocalProjectValidator> = Arc::new(fake.clone());
    let store = store(&temp);
    store.add("root", &root, false).expect("add");
    fake.unavailable(root.canonicalize().expect("canonical root"));
    let resolver = LocalProjectResolver::new(store.clone(), validator);
    let listed = resolver.list().expect("list");
    assert_eq!(listed.projects[0].status, LocalProjectStatus::Unavailable);
    assert!(listed.projects[0]
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("analyst")));
    assert!(matches!(
        resolver.resolve("root"),
        Err(LocalProjectError::ProjectUnavailable { .. })
    ));
    assert_eq!(store.snapshot().expect("snapshot").projects.len(), 1);
}

#[test]
fn live_list_is_sorted_even_when_valid_catalog_file_is_not() {
    let temp = tempfile::tempdir().expect("tempdir");
    let alpha = make_root(&temp, "alpha");
    let beta = make_root(&temp, "beta");
    let store = store(&temp);
    let path = store.catalog_path().expect("path");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &path,
        format!(
            "version = 1\ngeneration = 7\n\n[[projects]]\nname = \"beta\"\nroot = \"{}\"\n\n[[projects]]\nname = \"alpha\"\nroot = \"{}\"\n",
            beta.display(),
            alpha.display()
        ),
    )
    .expect("write unsorted catalog");
    let resolver = LocalProjectResolver::new(store, validator());

    let listed = resolver.list().expect("list");

    assert_eq!(
        listed
            .projects
            .iter()
            .map(|project| project.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta"]
    );
}

#[test]
fn routing_helpers_preserve_default_contract_and_propagate_explicit_scope() {
    let schema = json!({
        "type": "object",
        "properties": {"query": {"type": "string"}},
        "required": ["query"],
        "additionalProperties": false
    });
    let decorated = with_optional_project_schema(&schema);
    assert_eq!(decorated["required"], schema["required"]);
    assert_eq!(decorated["additionalProperties"], false);
    assert_eq!(decorated["properties"]["project"]["type"], "string");

    let temp = tempfile::tempdir().expect("tempdir");
    let root = make_root(&temp, "root");
    let store = store(&temp);
    store.add("alpha", &root, false).expect("add");
    let resolver = LocalProjectResolver::new(store, validator());
    let mut args = json!({"query": "Thing", "project": "alpha"});
    let resolved = extract_project(&mut args, &LocalProjectAccess::Catalog(resolver))
        .expect("extract")
        .expect("resolved");
    assert_eq!(args, json!({"query": "Thing"}));

    let body = json!({
        "answerable": true,
        "next": [
            {"tool": "code_read_symbol", "selector": "graph://symbol/1"},
            {"tool": "code_semantic_search", "query": "Thing"}
        ],
        "primary_evidence": [{"next": [{"tool": "code_callers"}]}],
        "recommended_next_tools": [{"tool": "code_callees"}],
        "user_content": {"next": "leave this scalar alone"}
    });
    let scoped = decorate_project_response(body, Some(&resolved));
    assert_eq!(scoped["project"]["name"], "alpha");
    assert_eq!(scoped["project"]["catalog_generation"], 1);
    assert_eq!(scoped["next"].as_array().expect("next array").len(), 1);
    assert_eq!(scoped["next"][0]["project"], "alpha");
    assert_eq!(scoped["primary_evidence"][0]["next"][0]["project"], "alpha");
    assert_eq!(scoped["recommended_next_tools"][0]["project"], "alpha");
    assert_eq!(scoped["user_content"]["next"], "leave this scalar alone");

    let unrelated = json!({"user_content": {"next": [{"value": 1}]}});
    let scoped = decorate_project_response(unrelated, Some(&resolved));
    assert_eq!(
        scoped["user_content"]["next"],
        json!([{"value": 1}]),
        "unrelated result content must not be rewritten"
    );

    let unscoped = decorate_project_response(json!({"answerable": true}), None);
    assert_eq!(unscoped, json!({"answerable": true}));
}

#[test]
fn project_blind_and_invalid_selectors_fail_closed() {
    let mut blind = json!({"project": "alpha"});
    let error = extract_project(&mut blind, &LocalProjectAccess::CurrentWorktreeOnly)
        .expect_err("project-blind access must reject selector");
    assert_eq!(error.json_rpc_code(), -32602);

    let temp = tempfile::tempdir().expect("tempdir");
    let resolver = LocalProjectResolver::new(store(&temp), validator());
    for value in [json!(7), json!("bad name")] {
        let mut args = json!({"project": value});
        assert!(
            extract_project(&mut args, &LocalProjectAccess::Catalog(resolver.clone())).is_err()
        );
    }
    let mut unknown = json!({"project": "missing"});
    let error = extract_project(&mut unknown, &LocalProjectAccess::Catalog(resolver))
        .expect_err("unknown project");
    assert_eq!(error.json_rpc_code(), -32004);
}

#[tokio::test]
async fn management_module_exposes_and_executes_exact_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = make_root(&temp, "root");
    let module = LocalProjectCatalogMcpModule::new(store(&temp), validator());
    let names = module
        .tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "local_project_add",
            "local_project_list",
            "local_project_remove"
        ]
    );
    let registry = ToolRegistry::builder()
        .with(module)
        .expect("register module")
        .build();
    let context = || ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None);
    let added = registry
        .call_json_tool(
            context(),
            "local_project_add",
            json!({"name": "alpha", "path": root, "replace": false}),
        )
        .await;
    let added = serde_json::to_value(added).expect("serialize response");
    let text = added["result"]["content"][0]["text"]
        .as_str()
        .expect("response text");
    let body: serde_json::Value = serde_json::from_str(text).expect("response body");
    assert_eq!(body["changed"], true);
    assert_eq!(body["project"]["status"], "ready");

    let listed = registry
        .call_json_tool(context(), "local_project_list", json!({}))
        .await;
    let listed = serde_json::to_value(listed).expect("serialize response");
    let text = listed["result"]["content"][0]["text"]
        .as_str()
        .expect("response text");
    let body: serde_json::Value = serde_json::from_str(text).expect("response body");
    assert_eq!(body["projects"][0]["name"], "alpha");

    let removed = registry
        .call_json_tool(context(), "local_project_remove", json!({"name": "alpha"}))
        .await;
    let removed = serde_json::to_value(removed).expect("serialize response");
    let text = removed["result"]["content"][0]["text"]
        .as_str()
        .expect("response text");
    let body: serde_json::Value = serde_json::from_str(text).expect("response body");
    assert_eq!(body["removed"], true);
}

#[tokio::test]
async fn management_module_rejects_unknown_fields_and_non_empty_list_inputs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = make_root(&temp, "root");
    let registry = ToolRegistry::builder()
        .with(LocalProjectCatalogMcpModule::new(store(&temp), validator()))
        .expect("register module")
        .build();
    let context = || ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None);

    for (tool, args) in [
        (
            "local_project_add",
            json!({"name": "alpha", "path": root, "unexpected": true}),
        ),
        (
            "local_project_remove",
            json!({"name": "alpha", "unexpected": true}),
        ),
        ("local_project_list", json!({"unexpected": true})),
        ("local_project_list", json!([])),
    ] {
        let response = registry.call_json_tool(context(), tool, args).await;
        let response = serde_json::to_value(response).expect("serialize response");
        assert_eq!(response["error"]["code"], -32602, "{tool}: {response}");
    }
}
