use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::path::Path;

use spur_core::explore::apply::{self, Resolution, Selection};
use spur_core::explore::catalog::{Catalog, CatalogEntry, ItemKind};
use spur_core::explore::pool::{self, Manifest};
use spur_core::explore::sync;

#[derive(Subcommand, Debug, Clone)]
pub enum ExploreCommands {
    /// Fetch pinned sources and rebuild the catalog index
    Sync,
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
        ExploreCommands::Sync => run_sync(repo_root),
        ExploreCommands::List {
            pool,
            agents,
            skills,
        } => run_list(repo_root, pool, agents, skills),
        ExploreCommands::Add {
            name,
            override_gate,
            replace_bundled,
        } => run_add(repo_root, &name, override_gate.as_deref(), replace_bundled),
        ExploreCommands::Remove { name } => run_remove(repo_root, &name),
        ExploreCommands::Status => run_status(repo_root),
    }
}

pub fn run_sync(repo_root: &Path) -> Result<()> {
    let manifest = Manifest::load(repo_root)?;
    let catalog = sync::sync(repo_root, &manifest)?;
    println!("synced {} explore entries", catalog.entries.len());
    Ok(())
}

pub fn run_list(repo_root: &Path, pool: bool, agents: bool, skills: bool) -> Result<()> {
    if pool {
        let manifest = Manifest::load(repo_root)?;
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
    let mut manifest = Manifest::load(repo_root)?;
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
    let outcome = apply::apply(
        repo_root,
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
    let mut manifest = Manifest::load(repo_root)?;
    apply::remove(repo_root, &mut manifest, name)?;
    println!("removed {name}");
    Ok(())
}

pub fn run_status(repo_root: &Path) -> Result<()> {
    let manifest = Manifest::load(repo_root)?;
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
    let path = catalog_path(repo_root);
    if !path.is_file() {
        bail!("explore catalog index is missing; run `spur explore sync` first");
    }
    Catalog::load(repo_root)
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
