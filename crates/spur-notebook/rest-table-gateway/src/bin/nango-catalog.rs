use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use spur_rest_table_gateway::adapter::catalog::{
    build_crosswalk_report, candidate_provider_blocked_reason, generate_candidate_manifest,
    generate_reviewed_manifest, provider_catalog_from_yaml, ApisGuruSnapshot, CrosswalkDiagnostics,
    CrosswalkOptions, ProviderCatalogEntry, ProviderFulfillmentStatus, ProviderSeedClass,
    ProviderSpecCrosswalk, APIS_GURU_CROSSWALK_CSV, API_GURU_FULFILLMENT_MATRIX_JSON,
    COVERAGE_SUMMARY_JSON, PROVIDER_HARVEST_CANDIDATES_CSV, PROVIDER_SPEC_CROSSWALK_JSON,
    TABLE_SEED_CLASSES_CSV,
};
use spur_rest_table_gateway::adapter::default_http_client;
use spur_rest_table_gateway::adapter::manifest::Manifest;
use spur_rest_table_gateway::adapter::nango::parse_providers;

const NANGO_LICENSE: &str = "Elastic License 2.0";
const EXPERIMENTAL_MANIFEST_INDEX_JSON: &str = "experimental_manifest_index.json";
const SUPPORTED_CONNECTIONS_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/connections/supported");
const USAGE: &str = "usage: nango-catalog <providers.yaml> <apis-guru-list.json> <out_dir> --nango-commit <sha> --apis-guru-fetched-at <timestamp> [--reviewed-source <provider>=<spec-path>] [--experimental-crosswalk-manifests]";

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
    let candidate_generations = if args.experimental_crosswalk_manifests {
        Some(build_candidate_generations(&providers_yaml, &report.rows)?)
    } else {
        None
    };
    let fulfillment_matrix = build_fulfillment_matrix(
        &providers_yaml,
        &report.rows,
        candidate_generations.as_deref(),
    )?;
    write_json(
        &args.out_dir.join(API_GURU_FULFILLMENT_MATRIX_JSON),
        &fulfillment_matrix,
    )?;
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
    if args.experimental_crosswalk_manifests {
        write_experimental_crosswalk_manifests(
            &args.out_dir,
            &report.rows,
            candidate_generations.as_deref().expect(
                "candidate generations should be present when experimental output is enabled",
            ),
        )?;
    }

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
    experimental_crosswalk_manifests: bool,
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
    let mut experimental_crosswalk_manifests = false;

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
            "--experimental-crosswalk-manifests" => {
                experimental_crosswalk_manifests = true;
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
        experimental_crosswalk_manifests,
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

fn build_fulfillment_matrix(
    providers_yaml: &str,
    rows: &[ProviderSpecCrosswalk],
    candidate_generations: Option<&[CandidateGeneration]>,
) -> Result<ApiGuruFulfillmentMatrix, Box<dyn Error>> {
    let providers = parse_providers(providers_yaml)?;
    let supported = load_supported_manifest_summaries()?;
    let candidate_paths = candidate_manifest_paths(rows);
    let mut matrix_rows = Vec::with_capacity(rows.len());

    for (index, (row, candidate_path)) in rows.iter().zip(candidate_paths).enumerate() {
        if let Some(summary) = supported_manifest_for(&supported, &row.provider) {
            matrix_rows.push(ApiGuruFulfillmentRow {
                provider_key: row.provider.clone(),
                spec_source_key: row.spec_source_key.clone(),
                spec_url: row.url.clone(),
                status: ProviderFulfillmentStatus::Ready,
                blocked_reason: None,
                supported_manifest: Some(summary.path.clone()),
                candidate_manifest: None,
                table_count: summary.table_count,
                action_count: summary.action_count,
            });
            continue;
        }

        if let Some(candidate_generations) = candidate_generations {
            let generation = candidate_generations
                .get(index)
                .ok_or("candidate generation count did not match crosswalk rows")?;
            match &generation.outcome {
                CandidateGenerationOutcome::Candidate { table_count, .. } => {
                    matrix_rows.push(ApiGuruFulfillmentRow {
                        provider_key: row.provider.clone(),
                        spec_source_key: row.spec_source_key.clone(),
                        spec_url: row.url.clone(),
                        status: ProviderFulfillmentStatus::Candidate,
                        blocked_reason: None,
                        supported_manifest: None,
                        candidate_manifest: Some(generation.path.clone()),
                        table_count: *table_count,
                        action_count: 0,
                    });
                }
                CandidateGenerationOutcome::Blocked { reason } => {
                    matrix_rows.push(blocked_fulfillment_row(row, reason));
                }
            }
            continue;
        }

        let Some(provider_entry) = providers.get(&row.provider) else {
            matrix_rows.push(blocked_fulfillment_row(row, "missing_provider"));
            continue;
        };

        if provider_entry
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.base_url.as_deref())
            .is_none_or(|base_url| base_url.trim().is_empty())
        {
            matrix_rows.push(blocked_fulfillment_row(row, "missing_base_url"));
            continue;
        }

        matrix_rows.push(ApiGuruFulfillmentRow {
            provider_key: row.provider.clone(),
            spec_source_key: row.spec_source_key.clone(),
            spec_url: row.url.clone(),
            status: ProviderFulfillmentStatus::Candidate,
            blocked_reason: None,
            supported_manifest: None,
            candidate_manifest: Some(candidate_path),
            table_count: 0,
            action_count: 0,
        });
    }

    Ok(ApiGuruFulfillmentMatrix {
        provider_count: matrix_rows
            .iter()
            .map(|row| row.provider_key.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        spec_row_count: matrix_rows.len(),
        rows: matrix_rows,
    })
}

fn blocked_fulfillment_row(row: &ProviderSpecCrosswalk, reason: &str) -> ApiGuruFulfillmentRow {
    ApiGuruFulfillmentRow {
        provider_key: row.provider.clone(),
        spec_source_key: row.spec_source_key.clone(),
        spec_url: row.url.clone(),
        status: ProviderFulfillmentStatus::Blocked,
        blocked_reason: Some(reason.to_string()),
        supported_manifest: None,
        candidate_manifest: None,
        table_count: 0,
        action_count: 0,
    }
}

fn load_supported_manifest_summaries(
) -> Result<BTreeMap<String, SupportedManifestSummary>, Box<dyn Error>> {
    let mut paths = fs::read_dir(SUPPORTED_CONNECTIONS_DIR)
        .map_err(|err| format!("failed to read {SUPPORTED_CONNECTIONS_DIR}: {err}"))?
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort_by_key(|entry| entry.path());

    let mut supported = BTreeMap::new();
    for entry in paths {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }

        let toml = fs::read_to_string(&path)
            .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
        let manifest = Manifest::from_toml(&toml).map_err(|err| {
            format!(
                "supported manifest {} failed to parse: {err}",
                path.display()
            )
        })?;
        let file_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .ok_or_else(|| format!("supported manifest path is not UTF-8: {}", path.display()))?;
        let file_stem = file_name
            .strip_suffix(".connection.toml")
            .ok_or_else(|| format!("unsupported manifest filename: {file_name}"))?;
        let summary = SupportedManifestSummary {
            path: format!("connections/supported/{file_name}"),
            table_count: manifest.tables.len(),
            action_count: manifest.actions.len(),
        };
        supported.insert(manifest.source.name, summary.clone());
        supported.insert(file_stem.to_string(), summary);
    }

    Ok(supported)
}

fn supported_manifest_for<'a>(
    supported: &'a BTreeMap<String, SupportedManifestSummary>,
    provider: &str,
) -> Option<&'a SupportedManifestSummary> {
    supported
        .get(provider)
        .or_else(|| supported_provider_alias(provider).and_then(|alias| supported.get(alias)))
}

fn supported_provider_alias(provider: &str) -> Option<&'static str> {
    match provider {
        "github-pat" => Some("github"),
        "sendgrid-api-key" => Some("sendgrid"),
        "stripe-api-key" => Some("stripe"),
        _ => None,
    }
}

fn build_candidate_generations(
    providers_yaml: &str,
    rows: &[ProviderSpecCrosswalk],
) -> Result<Vec<CandidateGeneration>, Box<dyn Error>> {
    let providers = parse_providers(providers_yaml)?;
    let candidate_paths = candidate_manifest_paths(rows);
    let mut generations = Vec::with_capacity(rows.len());

    for (row, path) in rows.iter().zip(candidate_paths) {
        let outcome = match providers.get(&row.provider) {
            Some(provider_entry) => {
                if let Some(reason) = candidate_provider_blocked_reason(provider_entry) {
                    CandidateGenerationOutcome::Blocked {
                        reason: reason.as_str().to_string(),
                    }
                } else {
                    match read_spec_text(&row.url) {
                        Ok(spec_text) => match generate_candidate_manifest(
                            &row.provider,
                            provider_entry,
                            &row.spec_source_key,
                            &row.url,
                            "Candidate",
                            &spec_text,
                        ) {
                            Ok(generated) => CandidateGenerationOutcome::Candidate {
                                toml: generated.toml,
                                table_count: generated.table_count,
                            },
                            Err(reason) => CandidateGenerationOutcome::Blocked {
                                reason: reason.as_str().to_string(),
                            },
                        },
                        Err(_) => CandidateGenerationOutcome::Blocked {
                            reason: "parse_failure".to_string(),
                        },
                    }
                }
            }
            None => CandidateGenerationOutcome::Blocked {
                reason: "missing_provider".to_string(),
            },
        };

        generations.push(CandidateGeneration { path, outcome });
    }

    Ok(generations)
}

fn read_spec_text(url: &str) -> Result<String, Box<dyn Error>> {
    if let Some(path) = url.strip_prefix("file://") {
        return fs::read_to_string(path)
            .map_err(|err| format!("failed to read spec {url}: {err}").into());
    }

    if url.starts_with("http://") || url.starts_with("https://") {
        return fetch_http_spec_text(url);
    }

    fs::read_to_string(url).map_err(|err| format!("failed to read spec {url}: {err}").into())
}

fn fetch_http_spec_text(url: &str) -> Result<String, Box<dyn Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("failed to create runtime for spec fetch: {err}"))?;
    let client = default_http_client();

    runtime.block_on(async move {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|err| format!("failed to fetch spec {url}: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("failed to fetch spec {url}: HTTP {status}").into());
        }
        response
            .text()
            .await
            .map_err(|err| format!("failed to read spec {url}: {err}").into())
    })
}

fn write_experimental_crosswalk_manifests(
    out_dir: &Path,
    rows: &[ProviderSpecCrosswalk],
    candidate_generations: &[CandidateGeneration],
) -> Result<(), Box<dyn Error>> {
    let connections_dir = out_dir.join("connections").join("experimental");
    fs::create_dir_all(&connections_dir)
        .map_err(|err| format!("failed to create {}: {err}", connections_dir.display()))?;

    let mut manifests = Vec::new();

    for (row, generation) in rows.iter().zip(candidate_generations) {
        let CandidateGenerationOutcome::Candidate { toml, .. } = &generation.outcome else {
            continue;
        };

        let full_path = out_dir.join(&generation.path);
        fs::write(&full_path, toml)
            .map_err(|err| format!("failed to write {}: {err}", full_path.display()))?;
        manifests.push(ExperimentalManifestIndexEntry {
            provider: &row.provider,
            spec_source_key: &row.spec_source_key,
            path: generation.path.clone(),
            confidence: format!("{:?}", row.confidence),
            license_status: format!("{:?}", row.license_status),
            generation_eligible: row.generation_eligible,
        });
    }

    write_json(
        &out_dir.join(EXPERIMENTAL_MANIFEST_INDEX_JSON),
        &ExperimentalManifestIndex {
            experimental: true,
            support_level: "experimental_crosswalk",
            crosswalk_row_count: rows.len(),
            manifest_count: manifests.len(),
            manifests,
        },
    )?;

    Ok(())
}

fn candidate_manifest_paths(rows: &[ProviderSpecCrosswalk]) -> Vec<String> {
    let mut paths = Vec::with_capacity(rows.len());
    let mut seen_paths = BTreeMap::<String, usize>::new();

    for row in rows {
        let mut file_stem = format!(
            "{}--{}",
            sanitize_filename_component(&row.provider),
            sanitize_filename_component(&row.spec_source_key)
        );
        let base_file_stem = file_stem.clone();
        let next = seen_paths.entry(base_file_stem.clone()).or_insert(0);
        if *next > 0 {
            file_stem = format!("{base_file_stem}-{}", *next + 1);
        }
        *next += 1;

        paths.push(format!(
            "connections/experimental/{file_stem}.connection.toml"
        ));
    }

    paths
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

fn sanitize_filename_component(value: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;
    for character in value.chars() {
        let next = if character.is_ascii_alphanumeric() || character == '.' {
            previous_dash = false;
            Some(character.to_ascii_lowercase())
        } else if !previous_dash {
            previous_dash = true;
            Some('-')
        } else {
            None
        };
        if let Some(next) = next {
            out.push(next);
        }
    }

    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
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

#[derive(Debug, Serialize)]
struct ApiGuruFulfillmentMatrix {
    provider_count: usize,
    spec_row_count: usize,
    rows: Vec<ApiGuruFulfillmentRow>,
}

#[derive(Debug, Serialize)]
struct ApiGuruFulfillmentRow {
    provider_key: String,
    spec_source_key: String,
    spec_url: String,
    status: ProviderFulfillmentStatus,
    blocked_reason: Option<String>,
    supported_manifest: Option<String>,
    candidate_manifest: Option<String>,
    table_count: usize,
    action_count: usize,
}

#[derive(Debug, Clone)]
struct SupportedManifestSummary {
    path: String,
    table_count: usize,
    action_count: usize,
}

#[derive(Debug, Clone)]
struct CandidateGeneration {
    path: String,
    outcome: CandidateGenerationOutcome,
}

#[derive(Debug, Clone)]
enum CandidateGenerationOutcome {
    Candidate { toml: String, table_count: usize },
    Blocked { reason: String },
}

#[derive(Debug, Serialize)]
struct ExperimentalManifestIndex<'a> {
    experimental: bool,
    support_level: &'a str,
    crosswalk_row_count: usize,
    manifest_count: usize,
    manifests: Vec<ExperimentalManifestIndexEntry<'a>>,
}

#[derive(Debug, Serialize)]
struct ExperimentalManifestIndexEntry<'a> {
    provider: &'a str,
    spec_source_key: &'a str,
    path: String,
    confidence: String,
    license_status: String,
    generation_eligible: bool,
}
