#![expect(
    dead_code,
    reason = "the shared projection fixture is consumed incrementally by Tasks 3 and 4"
)]

use crate::explore::catalog::ItemKind;
use crate::explore::pool::{pool_dir, GateRecord, Manifest, ManifestItem};
use crate::skills::adapters::Adapter;
use std::path::Path;

pub struct ProjectionFixture {
    repo: tempfile::TempDir,
    assets: tempfile::TempDir,
    launch: tempfile::TempDir,
    adapter: Adapter,
    worktrees: spur_worktree::manager::WorktreeManager,
}

impl ProjectionFixture {
    pub fn new(adapter: Adapter) -> Self {
        let repo = tempfile::tempdir().expect("create projection source repository");
        let assets = tempfile::tempdir().expect("create bundled skill assets");
        let launch = tempfile::tempdir().expect("create projection launch root");
        init_git_repo(repo.path());
        init_git_repo(launch.path());
        std::fs::create_dir_all(repo.path().join(".spur")).expect("create SPUR config directory");
        // Reconcile unit tests exercise full materialization of arbitrary skill
        // IDs; pin all_active so default foundation/catalog_only does not filter
        // them out (or fail closed without skills-catalog).
        std::fs::write(
            repo.path().join(".spur/config.toml"),
            format!(
                "[skills]\nbundled_dir = \"{}\"\nprojection_mode = \"all_active\"\n",
                toml_path(assets.path())
            ),
        )
        .expect("write bundled skill configuration");
        let worktrees = spur_worktree::manager::WorktreeManager::new(launch.path().to_path_buf());
        Self {
            repo,
            assets,
            launch,
            adapter,
            worktrees,
        }
    }

    pub fn repo_root(&self) -> &Path {
        self.repo.path()
    }

    pub fn launch_root(&self) -> &Path {
        self.launch.path()
    }

    pub fn worktrees(&self) -> &spur_worktree::manager::WorktreeManager {
        &self.worktrees
    }

    pub fn request(&self) -> super::ProjectionRequest<'_> {
        super::ProjectionRequest {
            source_repo_root: self.repo.path(),
            launch_root: self.launch.path(),
            adapter: self.adapter,
            role: super::RuntimeRole::Init,
            policy: super::SelectionPolicy::AllActive,
        }
    }

    pub fn write_bundled_skill(&self, id: &str, role: &str, body: &str) {
        write_skill_source(&self.assets.path().join(id), id, role, body);
    }

    pub fn write_repository_override(&self, id: &str, role: &str, body: &str) {
        write_skill_source(
            &self.repo.path().join(".spur/skills").join(id),
            id,
            role,
            body,
        );
    }

    pub fn write_pool_skill(&self, id: &str, verdict: &str, body: &str) {
        let source = "acme/skills";
        let pinned_commit = "abcdef1234567890abcdef1234567890abcdef12";
        let source_dir = pool_dir(self.repo.path(), source, id, pinned_commit);
        write_skill_source(&source_dir, id, "both", body);
        let content_sha256 =
            crate::explore::content_hash(&source_dir).expect("hash projection fixture pool source");
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
        let mut manifest = Manifest::load(self.repo.path()).expect("load projection fixture pool");
        manifest.items.retain(|existing| existing.name != id);
        manifest.items.push(item);
        manifest
            .save(self.repo.path())
            .expect("save projection fixture pool");
    }

    pub fn write_support(&self, id: &str, relative: &str, bytes: &[u8]) {
        let path = self.assets.path().join(id).join(relative);
        std::fs::create_dir_all(path.parent().expect("support file has parent"))
            .expect("create projection support directory");
        std::fs::write(path, bytes).expect("write projection support file");
    }

    pub fn resolve(
        &self,
    ) -> Result<Vec<super::resolver::ResolvedSkill>, super::resolver::ResolveError> {
        super::resolver::resolve_effective_skills(
            self.repo.path(),
            self.adapter,
            super::RuntimeRole::Init,
            super::SelectionPolicy::AllActive,
        )
    }
}

fn write_skill_source(source_dir: &Path, id: &str, role: &str, body: &str) {
    std::fs::create_dir_all(source_dir).expect("create projection fixture skill directory");
    std::fs::write(
        source_dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: {id} description\nrole: {role}\n---\n{body}"),
    )
    .expect("write projection fixture skill");
}

fn init_git_repo(path: &Path) {
    run_git(path, &["init", "--quiet"]);
    run_git(path, &["config", "user.email", "projection@example.com"]);
    run_git(path, &["config", "user.name", "Projection Fixture"]);
}

fn run_git(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run Git for projection fixture");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}
