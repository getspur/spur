use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use spur_rest_table_gateway::adapter::catalog::{
    build_crosswalk_report, generate_reviewed_manifest, provider_catalog_from_yaml,
    ApisGuruSnapshot, CrosswalkDiagnostics, CrosswalkOptions, ProviderCatalogEntry,
    ProviderSeedClass, ProviderSpecCrosswalk, APIS_GURU_CROSSWALK_CSV, COVERAGE_SUMMARY_JSON,
    PROVIDER_HARVEST_CANDIDATES_CSV, PROVIDER_SPEC_CROSSWALK_JSON, TABLE_SEED_CLASSES_CSV,
};
use spur_rest_table_gateway::adapter::nango::parse_providers;

const NANGO_LICENSE: &str = "Elastic License 2.0";
const USAGE: &str = "usage: nango-catalog <providers.yaml> <apis-guru-list.json> <out_dir> --nango-commit <sha> --apis-guru-fetched-at <timestamp> [--reviewed-source <provider>=<spec-path>]";

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        eprintln!("{USAGE}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args = parse_args(env::args().skip(1))?;
    let providers_yaml = fs::read_to_string(&args.providers_yaml)
        .map_err(|err| format!("failed to read {}: {err}", args.providers_yaml.display()))?;
    let apis_guru_json = fs::read_to_string(&args.apis_guru_list)
        .map_err(|err| format!("failed to read {}: {err}", args.apis_guru_list.display()))?;

    let providers = provider_catalog_from_yaml(&providers_yaml, &args.nango_commit)
        .map_err(|err| format!("failed to parse {}: {err}", args.providers_yaml.display()))?;
    let apis_guru = ApisGuruSnapshot::parse(&apis_guru_json, &args.apis_guru_fetched_at)
        .map_err(|err| format!("failed to parse {}: {err}", args.apis_guru_list.display()))?;
    let report = build_crosswalk_report(
        &providers,
        &apis_guru.sources,
        CrosswalkOptions {
            apis_guru_hash: Some(apis_guru.sha256.clone()),
        },
    );
    let metadata = CatalogMetadata {
        nango_license: NANGO_LICENSE,
        nango_commit: &args.nango_commit,
        apis_guru_retrieved_at: &apis_guru.retrieved_at,
        apis_guru_sha256: &apis_guru.sha256,
    };

    fs::create_dir_all(&args.out_dir)
        .map_err(|err| format!("failed to create {}: {err}", args.out_dir.display()))?;
    write_provider_harvest(&args.out_dir, &providers)?;
    write_seed_classes(&args.out_dir, &report.diagnostics.providers_by_seed_class)?;
    write_apis_guru_crosswalk(&args.out_dir, &report.rows)?;
    write_json(
        &args.out_dir.join(PROVIDER_SPEC_CROSSWALK_JSON),
        &ProviderSpecCrosswalkArtifact {
            metadata,
            diagnostics: &report.diagnostics,
            rows: &report.rows,
        },
    )?;
    write_json(
        &args.out_dir.join(COVERAGE_SUMMARY_JSON),
        &CoverageSummary {
            metadata,
            provider_count: providers.len(),
            apis_guru_total_entries: apis_guru.total_entries,
            crosswalk_row_count: report.rows.len(),
            matched_provider_count: report.diagnostics.distinct_matched_providers,
            rejected_ambiguous_candidates: report.diagnostics.rejected_ambiguous_candidates,
            providers_by_seed_class: &report.diagnostics.providers_by_seed_class,
        },
    )?;
    write_reviewed_manifests(&args.out_dir, &providers_yaml, &args.reviewed_sources)?;

    println!(
        "wrote catalog for {} providers, {} APIs.guru entries, {} crosswalk rows",
        providers.len(),
        apis_guru.total_entries,
        report.rows.len()
    );

    Ok(())
}

#[derive(Debug)]
struct Args {
    providers_yaml: PathBuf,
    apis_guru_list: PathBuf,
    out_dir: PathBuf,
    nango_commit: String,
    apis_guru_fetched_at: String,
    reviewed_sources: Vec<ReviewedSourceArg>,
}

#[derive(Debug)]
struct ReviewedSourceArg {
    provider: String,
    spec_path: PathBuf,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, Box<dyn Error>> {
    let mut args = args.into_iter();
    let providers_yaml = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage_error("missing providers.yaml"))?;
    let apis_guru_list = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage_error("missing apis-guru-list.json"))?;
    let out_dir = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage_error("missing out_dir"))?;

    let mut nango_commit = None;
    let mut apis_guru_fetched_at = None;
    let mut reviewed_sources = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--nango-commit" => {
                let value = args
                    .next()
                    .ok_or_else(|| usage_error("--nango-commit requires a SHA"))?;
                if value.trim().is_empty() {
                    return Err(usage_error("--nango-commit requires a SHA"));
                }
                nango_commit = Some(value);
            }
            "--apis-guru-fetched-at" => {
                let value = args
                    .next()
                    .ok_or_else(|| usage_error("--apis-guru-fetched-at requires a timestamp"))?;
                if value.trim().is_empty() {
                    return Err(usage_error("--apis-guru-fetched-at requires a timestamp"));
                }
                apis_guru_fetched_at = Some(value);
            }
            "--reviewed-source" => {
                let value = args
                    .next()
                    .ok_or_else(|| usage_error("--reviewed-source requires provider=spec-path"))?;
                reviewed_sources.push(parse_reviewed_source(&value)?);
            }
            other if other.starts_with("--") => return Err(usage_error("unknown option")),
            _ => return Err(usage_error("unexpected argument")),
        }
    }

    Ok(Args {
        providers_yaml,
        apis_guru_list,
        out_dir,
        nango_commit: nango_commit.ok_or_else(|| usage_error("--nango-commit is required"))?,
        apis_guru_fetched_at: apis_guru_fetched_at
            .ok_or_else(|| usage_error("--apis-guru-fetched-at is required"))?,
        reviewed_sources,
    })
}

fn parse_reviewed_source(value: &str) -> Result<ReviewedSourceArg, Box<dyn Error>> {
    let (provider, spec_path) = value
        .split_once('=')
        .ok_or_else(|| usage_error("--reviewed-source requires provider=spec-path"))?;
    if provider.trim().is_empty() || spec_path.trim().is_empty() {
        return Err(usage_error("--reviewed-source requires provider=spec-path"));
    }
    if spec_path.starts_with("http://") || spec_path.starts_with("https://") {
        return Err(usage_error("--reviewed-source requires a local spec path"));
    }

    Ok(ReviewedSourceArg {
        provider: provider.to_string(),
        spec_path: PathBuf::from(spec_path),
    })
}

fn write_reviewed_manifests(
    out_dir: &Path,
    providers_yaml: &str,
    reviewed_sources: &[ReviewedSourceArg],
) -> Result<(), Box<dyn Error>> {
    if reviewed_sources.is_empty() {
        return Ok(());
    }

    let providers = parse_providers(providers_yaml)?;
    let connections_dir = out_dir.join("connections");
    fs::create_dir_all(&connections_dir)
        .map_err(|err| format!("failed to create {}: {err}", connections_dir.display()))?;

    for reviewed_source in reviewed_sources {
        let provider_entry = providers.get(&reviewed_source.provider).ok_or_else(|| {
            format!(
                "--reviewed-source provider {} is not present in providers.yaml",
                reviewed_source.provider
            )
        })?;
        let spec_text = fs::read_to_string(&reviewed_source.spec_path).map_err(|err| {
            format!(
                "failed to read reviewed source {}: {err}",
                reviewed_source.spec_path.display()
            )
        })?;
        let generated =
            generate_reviewed_manifest(&reviewed_source.provider, provider_entry, &spec_text)
                .map_err(|err| {
                    format!(
                        "failed to generate reviewed manifest for {} from {}: {err}",
                        reviewed_source.provider,
                        reviewed_source.spec_path.display()
                    )
                })?;
        fs::write(
            connections_dir.join(format!("{}.connection.toml", reviewed_source.provider)),
            generated.toml,
        )?;
    }

    Ok(())
}

fn write_provider_harvest(
    out_dir: &Path,
    providers: &[ProviderCatalogEntry],
) -> Result<(), Box<dyn Error>> {
    let mut csv = String::from(
        "provider,display_name,auth_mode,base_url,seed_class,categories,nango_license,nango_commit\n",
    );
    for provider in providers {
        push_csv_row(
            &mut csv,
            [
                provider.provider.as_str(),
                provider.display_name.as_str(),
                provider.auth_mode.as_deref().unwrap_or_default(),
                provider.base_url.as_deref().unwrap_or_default(),
                seed_class_name(provider.seed_class),
                &provider.categories.join(";"),
                provider.nango_license.as_str(),
                provider.nango_commit.as_str(),
            ],
        );
    }
    fs::write(out_dir.join(PROVIDER_HARVEST_CANDIDATES_CSV), csv)?;
    Ok(())
}

fn write_seed_classes(
    out_dir: &Path,
    seed_classes: &BTreeMap<ProviderSeedClass, usize>,
) -> Result<(), Box<dyn Error>> {
    let mut csv = String::from("seed_class,count\n");
    for (seed_class, count) in seed_classes {
        push_csv_row(&mut csv, [seed_class_name(*seed_class), &count.to_string()]);
    }
    fs::write(out_dir.join(TABLE_SEED_CLASSES_CSV), csv)?;
    Ok(())
}

fn write_apis_guru_crosswalk(
    out_dir: &Path,
    rows: &[ProviderSpecCrosswalk],
) -> Result<(), Box<dyn Error>> {
    let mut csv = String::from(
        "provider,spec_source_key,confidence,url,match_reasons,license_status,generation_eligible\n",
    );
    for row in rows {
        push_csv_row(
            &mut csv,
            [
                row.provider.as_str(),
                row.spec_source_key.as_str(),
                &format!("{:?}", row.confidence),
                row.url.as_str(),
                &row.match_reasons.join(";"),
                &format!("{:?}", row.license_status),
                if row.generation_eligible {
                    "true"
                } else {
                    "false"
                },
            ],
        );
    }
    fs::write(out_dir.join(APIS_GURU_CROSSWALK_CSV), csv)?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    let json = serde_json::to_string_pretty(value)?;
    fs::write(path, format!("{json}\n"))?;
    Ok(())
}

fn push_csv_row<'a>(csv: &mut String, fields: impl IntoIterator<Item = &'a str>) {
    let mut first = true;
    for field in fields {
        if !first {
            csv.push(',');
        }
        first = false;
        push_csv_field(csv, field);
    }
    csv.push('\n');
}

fn push_csv_field(csv: &mut String, field: &str) {
    if field.contains([',', '"', '\n', '\r']) {
        csv.push('"');
        for character in field.chars() {
            if character == '"' {
                csv.push('"');
            }
            csv.push(character);
        }
        csv.push('"');
    } else {
        csv.push_str(field);
    }
}

fn seed_class_name(seed_class: ProviderSeedClass) -> &'static str {
    match seed_class {
        ProviderSeedClass::BaseUrlOnly => "BaseUrlOnly",
        ProviderSeedClass::RestCollectionLikeDocsEndpoint => "RestCollectionLikeDocsEndpoint",
        ProviderSeedClass::RestSingletonOrUnknownDocsEndpoint => {
            "RestSingletonOrUnknownDocsEndpoint"
        }
        ProviderSeedClass::VerificationEndpointOnly => "VerificationEndpointOnly",
        ProviderSeedClass::GraphqlCandidate => "GraphqlCandidate",
        ProviderSeedClass::MetadataOnly => "MetadataOnly",
    }
}

fn usage_error(message: &'static str) -> Box<dyn Error> {
    message.into()
}

#[derive(Debug, Clone, Copy, Serialize)]
struct CatalogMetadata<'a> {
    nango_license: &'a str,
    nango_commit: &'a str,
    apis_guru_retrieved_at: &'a str,
    apis_guru_sha256: &'a str,
}

#[derive(Debug, Serialize)]
struct ProviderSpecCrosswalkArtifact<'a> {
    metadata: CatalogMetadata<'a>,
    diagnostics: &'a CrosswalkDiagnostics,
    rows: &'a [ProviderSpecCrosswalk],
}

#[derive(Debug, Serialize)]
struct CoverageSummary<'a> {
    metadata: CatalogMetadata<'a>,
    provider_count: usize,
    apis_guru_total_entries: usize,
    crosswalk_row_count: usize,
    matched_provider_count: usize,
    rejected_ambiguous_candidates: usize,
    providers_by_seed_class: &'a BTreeMap<ProviderSeedClass, usize>,
}
