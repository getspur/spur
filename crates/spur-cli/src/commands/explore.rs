use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::path::Path;

use spur_core::explore::apply::{self, Resolution, Selection};
use spur_core::explore::catalog::{Catalog, CatalogEntry, ItemKind};
use spur_core::explore::pool::{self, Manifest};
use spur_core::explore::store;
use spur_core::explore::sync;

#[derive(Subcommand, Debug, Clone)]
pub enum ExploreCommands {
    /// Fetch pinned sources and rebuild the catalog index
    Sync {
        /// Write catalog/cache to the shared user-wide explore store
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Write catalog/cache to this repository only
        #[arg(long, conflicts_with = "global")]
        local: bool,
    },
    /// List catalog entries (default) or --pool for adopted items
    List {
        #[arg(long)]
        pool: bool,
        #[arg(long)]
        agents: bool,
        #[arg(long)]
        skills: bool,
    },
    /// Gate + vendor an item into the pool
    Add {
        name: String,
        /// Write the manifest item to the shared user-wide explore manifest
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Write the manifest item to this repository only
        #[arg(long, conflicts_with = "global")]
        local: bool,
        /// Justification for accepting a flagged item
        #[arg(long = "override-gate", value_name = "JUSTIFICATION")]
        override_gate: Option<String>,
        /// Replace a bundled skill with the selected pool item
        #[arg(long)]
        replace_bundled: bool,
    },
    /// Remove an item from the pool
    Remove { name: String },
    /// Verify manifest/pool consistency and report drift
    Status,
}

pub fn run(cmd: ExploreCommands, repo_root: &Path) -> Result<()> {
    match cmd {
        ExploreCommands::Sync {
            global: false,
            local: false,
        } => run_sync(repo_root),
        ExploreCommands::Sync { global, local } => {
            run_sync_with_target(repo_root, sync_target(global, local)?)
        }
        ExploreCommands::List {
            pool,
            agents,
            skills,
        } => run_list(repo_root, pool, agents, skills),
        ExploreCommands::Add {
            name,
            global,
            local,
            override_gate,
            replace_bundled,
        } => {
            if !global && !local {
                run_add(repo_root, &name, override_gate.as_deref(), replace_bundled)
            } else {
                run_add_with_target(
                    repo_root,
                    &name,
                    manifest_target(global, local)?,
                    override_gate.as_deref(),
                    replace_bundled,
                )
            }
        }
        ExploreCommands::Remove { name } => run_remove(repo_root, &name),
        ExploreCommands::Status => run_status(repo_root),
    }
}

pub fn run_sync(repo_root: &Path) -> Result<()> {
    run_sync_with_target(repo_root, StoreTarget::Global)
}

pub fn run_sync_with_target(repo_root: &Path, target: StoreTarget) -> Result<()> {
    let manifest = Manifest::load_layered(repo_root)?;
    let store_root = store_root_for(repo_root, target)?;
    let catalog = sync::sync_to_store(&store_root, &manifest)?;
    println!("synced {} explore entries", catalog.entries.len());
    Ok(())
}

pub fn run_list(repo_root: &Path, pool: bool, agents: bool, skills: bool) -> Result<()> {
    if pool {
        let manifest = Manifest::load_layered(repo_root)?;
        for item in manifest
            .items
            .iter()
            .filter(|item| kind_matches(item.kind, agents, skills))
        {
            println!(
                "{} {} {}@{}",
                kind_label(item.kind),
                item.name,
                item.source,
                short_commit(&item.pinned_commit)
            );
        }
        return Ok(());
    }

    let catalog = load_synced_catalog(repo_root)?;
    for entry in catalog
        .entries
        .iter()
        .filter(|entry| kind_matches(entry.kind, agents, skills))
    {
        println!(
            "{} {} - {}",
            kind_label(entry.kind),
            entry.name,
            entry.description
        );
    }
    Ok(())
}

pub fn run_add(
    repo_root: &Path,
    name: &str,
    override_gate: Option<&str>,
    replace_bundled: bool,
) -> Result<()> {
    run_add_with_target(
        repo_root,
        name,
        StoreTarget::Local,
        override_gate,
        replace_bundled,
    )
}

pub fn run_add_with_target(
    repo_root: &Path,
    name: &str,
    target: StoreTarget,
    override_gate: Option<&str>,
    replace_bundled: bool,
) -> Result<()> {
    let mut manifest = load_manifest_for_target(repo_root, target)?;
    let catalog = load_synced_catalog(repo_root)?;
    let entry = find_entry(&catalog, name)?.clone();
    let resolution = match (override_gate, replace_bundled) {
        (Some(justification), _) => Resolution::Override {
            justification: justification.to_string(),
        },
        (None, true) => Resolution::ReplaceBundled,
        (None, false) => Resolution::Accept,
    };
    let bundled_ids = spur_core::skills::list_active_skills(repo_root)
        .context("load bundled skills for explore conflict checks")?
        .into_iter()
        .map(|skill| skill.id)
        .collect::<Vec<_>>();
    let pool_store_root = store_root_for(repo_root, StoreTarget::Global)?;
    let manifest_store_root = (target == StoreTarget::Global).then_some(pool_store_root.as_path());
    let outcome = apply::apply_with_store_roots(
        repo_root,
        &pool_store_root,
        manifest_store_root,
        &mut manifest,
        &[Selection { entry, resolution }],
        &bundled_ids,
    )?;

    for name in &outcome.installed {
        println!("installed {name}");
    }
    for (name, reason) in &outcome.skipped {
        println!("skipped {name}: {reason}");
    }

    if outcome.installed.is_empty() && !outcome.skipped.is_empty() {
        let reasons = outcome
            .skipped
            .iter()
            .map(|(name, reason)| format!("{name}: {reason}"))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("skipped {reasons}");
    }

    Ok(())
}

pub fn run_remove(repo_root: &Path, name: &str) -> Result<()> {
    let mut manifest = Manifest::load_layered(repo_root)?;
    apply::remove_layered(repo_root, &mut manifest, name)?;
    println!("removed {name}");
    Ok(())
}

pub fn run_status(repo_root: &Path) -> Result<()> {
    let manifest = Manifest::load_layered(repo_root)?;
    let report = pool::status(repo_root, &manifest);

    for name in &report.ok {
        println!("ok {name}");
    }
    for name in &report.missing {
        println!("missing {name}");
    }
    for name in &report.sha_mismatch {
        println!("sha_mismatch {name}");
    }

    if !report.missing.is_empty() || !report.sha_mismatch.is_empty() {
        bail!(
            "explore status failed: {} missing, {} sha_mismatch",
            report.missing.len(),
            report.sha_mismatch.len()
        );
    }

    println!("status ok ({} items)", report.ok.len());
    Ok(())
}

fn load_synced_catalog(repo_root: &Path) -> Result<Catalog> {
    if !has_catalog_index(repo_root) {
        bail!("explore catalog index is missing; run `spur explore sync` first");
    }
    Catalog::load_merged(repo_root)
}

fn find_entry<'a>(catalog: &'a Catalog, name: &str) -> Result<&'a CatalogEntry> {
    catalog
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .with_context(|| format!("explore catalog entry not found: {name}"))
}

fn catalog_path(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join(".spur/explore/index/catalog.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreTarget {
    Global,
    Local,
}

fn sync_target(global: bool, local: bool) -> Result<StoreTarget> {
    target_from_flags(global, local, StoreTarget::Global)
}

fn manifest_target(global: bool, local: bool) -> Result<StoreTarget> {
    target_from_flags(global, local, StoreTarget::Local)
}

fn target_from_flags(global: bool, local: bool, default: StoreTarget) -> Result<StoreTarget> {
    match (global, local) {
        (true, true) => bail!("--global and --local cannot be used together"),
        (true, false) => Ok(StoreTarget::Global),
        (false, true) => Ok(StoreTarget::Local),
        (false, false) => Ok(default),
    }
}

fn store_root_for(repo_root: &Path, target: StoreTarget) -> Result<std::path::PathBuf> {
    match target {
        StoreTarget::Global => {
            Ok(store::global_root().unwrap_or_else(|| store::local_root(repo_root)))
        }
        StoreTarget::Local => Ok(store::local_root(repo_root)),
    }
}

fn load_manifest_for_target(repo_root: &Path, target: StoreTarget) -> Result<Manifest> {
    match target {
        StoreTarget::Global => Manifest::load_from_store(&store_root_for(repo_root, target)?),
        StoreTarget::Local => Manifest::load(repo_root),
    }
}

fn has_catalog_index(repo_root: &Path) -> bool {
    catalog_path(repo_root).is_file()
        || store::global_root()
            .map(|root| store::catalog_path_in_store(&root).is_file())
            .unwrap_or(false)
}

fn kind_matches(kind: ItemKind, agents: bool, skills: bool) -> bool {
    (!agents && !skills)
        || (agents && kind == ItemKind::Agent)
        || (skills && kind == ItemKind::Skill)
}

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Agent => "agent",
        ItemKind::Skill => "skill",
    }
}

fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}
