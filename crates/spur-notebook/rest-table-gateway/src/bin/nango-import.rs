use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use spur_rest_table_gateway::adapter::nango::{
    manifest_to_toml, parse_providers, provider_to_manifest_stub, tier,
};

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args(env::args().skip(1))?;
    let yaml = fs::read_to_string(&args.providers_yaml)?;
    let providers = parse_providers(&yaml)?;

    fs::create_dir_all(&args.out_dir)?;

    let mut counts = TierCounts::default();
    let mut selected = 0usize;

    for (name, provider) in providers.iter() {
        let provider_tier = tier(provider.auth_mode.as_deref());
        if let Some(filter_tier) = args.tier {
            if provider_tier != filter_tier {
                continue;
            }
        }
        if !args.categories.is_empty()
            && !provider
                .categories
                .as_ref()
                .is_some_and(|categories| categories.iter().any(|c| args.categories.contains(c)))
        {
            continue;
        }
        if !args.names.is_empty() && !args.names.contains(name) {
            continue;
        }

        let manifest = provider_to_manifest_stub(name, provider);
        let path = args.out_dir.join(format!("{name}.connection.toml"));
        fs::write(path, manifest_to_toml(&manifest))?;
        counts.add(provider_tier);
        selected += 1;
    }

    println!(
        "selected {selected} providers: tier A={}, tier B={}, tier C={}",
        counts.a, counts.b, counts.c
    );

    Ok(())
}

#[derive(Debug)]
struct Args {
    providers_yaml: PathBuf,
    out_dir: PathBuf,
    tier: Option<char>,
    categories: BTreeSet<String>,
    names: BTreeSet<String>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, Box<dyn Error>> {
    let mut args = args.into_iter();
    let providers_yaml = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage_error("missing providers.yaml"))?;
    let out_dir = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage_error("missing out_dir"))?;

    let mut tier_filter = None;
    let mut categories = BTreeSet::new();
    let mut names = BTreeSet::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tier" => {
                let value = args
                    .next()
                    .ok_or_else(|| usage_error("--tier requires A, B, or C"))?;
                let tier = parse_tier(&value)?;
                tier_filter = Some(tier);
            }
            "--category" => {
                let value = args
                    .next()
                    .ok_or_else(|| usage_error("--category requires a comma-separated list"))?;
                categories.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|category| !category.is_empty())
                        .map(ToString::to_string),
                );
            }
            other if other.starts_with("--") => return Err(usage_error("unknown option")),
            name => {
                names.insert(name.to_string());
            }
        }
    }

    Ok(Args {
        providers_yaml,
        out_dir,
        tier: tier_filter,
        categories,
        names,
    })
}

fn parse_tier(value: &str) -> Result<char, Box<dyn Error>> {
    let mut chars = value.chars();
    let Some(tier) = chars.next() else {
        return Err(usage_error("--tier requires A, B, or C"));
    };
    if chars.next().is_some() {
        return Err(usage_error("--tier requires A, B, or C"));
    }

    match tier.to_ascii_uppercase() {
        'A' | 'B' | 'C' => Ok(tier.to_ascii_uppercase()),
        _ => Err(usage_error("--tier requires A, B, or C")),
    }
}

fn usage_error(message: &'static str) -> Box<dyn Error> {
    format!(
        "{message}\nusage: nango-import <providers.yaml> <out_dir> [--tier A] [--category dev-tools,analytics] [names...]"
    )
    .into()
}

#[derive(Default)]
struct TierCounts {
    a: usize,
    b: usize,
    c: usize,
}

impl TierCounts {
    fn add(&mut self, tier: char) {
        match tier {
            'A' => self.a += 1,
            'B' => self.b += 1,
            _ => self.c += 1,
        }
    }
}
