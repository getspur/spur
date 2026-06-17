use anyhow::Result;
use std::path::Path;

pub fn run(repo_root: &Path) -> Result<()> {
    let (cfg, sections, agents) = spur_acp::config::layered::effective_with_origins(repo_root)?;
    println!("# Effective SPUR config (Default < ~/.spur < .spur)");
    println!("# section origins:");
    for (key, origin) in &sections {
        println!("#   {key:<14} <- {origin}");
    }
    if !agents.is_empty() {
        println!("# agent origins:");
        for (name, origin) in &agents {
            println!("#   {name:<18} <- {origin}");
        }
    }
    println!();
    print!("{}", toml::to_string_pretty(&cfg)?);
    Ok(())
}
