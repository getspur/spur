use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{Duration, Instant},
};

use anyhow::{bail, ensure, Context, Result};
use chrono::{SecondsFormat, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use spur_graph::memory_eval::{
    artifacts::{
        ArtifactDigest, ArtifactWriter, MetricValue, QaArtifactKind, QaProgress, ReleaseGates,
        RetrievalGateEvidence, RetrievalMetrics, RunEvent, RunManifest, RunState,
    },
    contract::{
        validate_dataset, BenchmarkContract, BenchmarkDataset, DatasetKind, QuestionRecord,
        SessionRecord, SourcePin, ValidationReport,
    },
    memory_graph::{
        GraphIndexOnlyRanker, GraphTraversalRanker, MemoryGraph, MemoryRelation, MemorySession,
        MemoryTurn, TraversalConfig,
    },
    qa::{
        evaluate_locomo, evaluate_longmem, ranking_sha256, render_locomo_prompt_with_seed,
        JsonQaCache, LongMemQaRecord, OpenAiResponsesBackend, QaBackend, QaBudget, QaBudgetLimits,
        QaRecord, QaRequest, QaResponse, QaStatus, LONGMEMEVAL_MAX_INPUT_TOKENS, LONGMEMEVAL_MODEL,
        OPENAI_RESPONSES_URL,
    },
    ranking::{
        oracle_ranking, Bm25Ranker, ChronologyKey, CorpusDocument, Granularity, OracleRequest,
        QueryOccurrenceId, RankRequest, Ranker, Ranking, RankingSet, RecentRanker, Variant,
    },
};

// Task 7's scorer remains crate-private while Task 12 retains the legacy
// memory_eval exports. Include that exact implementation here instead of
// duplicating ranking/scoring logic in the Task 11 runner. The serialized
// report is converted to Task 8's public artifact type below.
#[allow(dead_code)]
mod task7_scoring {
    pub use spur_graph::memory_eval::{contract, ranking};
    pub const COVERED_WEIGHT: u32 = 1000;
    pub const PARTIAL_WEIGHT: u32 = 500;

    pub mod metrics {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/memory_eval/metrics.rs"
        ));
    }
}

use task7_scoring::metrics::{
    score_locomo_retrieval, score_longmemeval_retrieval, RetrievalMetricInput,
};

const CONTRACT_NAME: &str = "origin-faithful-v1";
const LOCOMO_ORIGIN: &str = "https://github.com/snap-research/locomo";
const LOCOMO_REVISION: &str = "3eb6f2c585f5e1699204e3c3bdf7adc5c28cb376";
const LOCOMO_SHA256: &str = "79fa87e90f04081343b8c8debecb80a9a6842b76a7aa537dc9fdf651ea698ff4";
const LONGMEMEVAL_ORIGIN: &str = "https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned";
const LONGMEMEVAL_REVISION: &str = "98d7416c24c778c2fee6e6f3006e7a073259d48f";
const LONGMEMEVAL_SHA256: &str = "d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442";
const LOCOMO_DEFAULT_K: usize = 10;
const LONGMEMEVAL_DEFAULT_K: usize = 50;
const DEFAULT_SEED_K: usize = 10;
const DEFAULT_MAX_DEPTH: usize = 3;
const TOKENS_RESERVED_PER_REQUEST: u64 = 200_000;
const INPUT_USD_MICROS_PER_MILLION: u64 = 2_500_000;
const OUTPUT_USD_MICROS_PER_MILLION: u64 = 10_000_000;
const LOCOMO_MAX_OUTPUT_TOKENS: u64 = 800;

const VARIANTS: [Variant; 5] = [
    Variant::Oracle,
    Variant::Recent,
    Variant::FlatBm25,
    Variant::GraphIndexOnly,
    Variant::GraphTraversal,
];

#[derive(Debug, Parser)]
#[command(name = "memory_benchmark")]
#[command(about = "Audited origin-faithful conversational-memory benchmark runner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate one canonical source and initialize a run directory.
    Validate(DatasetArgs),
    /// Validate, run all five retrieval variants, and publish retrieval-only output.
    Retrieve(RetrieveArgs),
    /// Run QA from immutable rankings, or leave the run QA-pending without authorization.
    Qa(QaArgs),
    /// Resume QA by question ID from immutable rankings and cache records.
    Resume(QaArgs),
    /// Regenerate the report from existing immutable run artifacts.
    Report(RunArgs),
}

#[derive(Debug, Clone, Args)]
struct DatasetArgs {
    /// LoCoMo JSON source.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "longmemeval",
        required_unless_present = "longmemeval"
    )]
    locomo: Option<PathBuf>,
    /// LongMemEval-S-cleaned JSON source.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "locomo",
        required_unless_present = "locomo"
    )]
    longmemeval: Option<PathBuf>,
    /// One run directory shared by validate/retrieve/qa/resume/report.
    #[arg(long, value_name = "DIR")]
    output: PathBuf,
    /// Explicit publication track. Only exact approved bytes qualify as audited.
    #[arg(long, value_enum)]
    track: SourceTrack,
    /// Optional revision label for compatibility or smoke inputs.
    #[arg(long)]
    source_revision: Option<String>,
    /// Unique canonical provenance results retained per ranking. Defaults to
    /// the largest dataset-native metric cutoff (LoCoMo 10, LongMemEval 50).
    #[arg(long)]
    k: Option<usize>,
    /// Lexical graph seeds used only by graph_traversal.
    #[arg(long, default_value_t = DEFAULT_SEED_K)]
    seed_k: usize,
    /// Traversal depth used only by graph_traversal.
    #[arg(long, default_value_t = DEFAULT_MAX_DEPTH)]
    max_depth: usize,
}

#[derive(Debug, Clone, Args)]
struct RetrieveArgs {
    #[command(flatten)]
    dataset: DatasetArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceTrack {
    Audited,
    Compatibility,
    Smoke,
}

#[derive(Debug, Clone, Args)]
struct QaArgs {
    /// Existing run directory containing frozen rankings.
    #[arg(long, value_name = "DIR")]
    output: PathBuf,
    /// Explicit authorization to make paid QA requests.
    #[arg(long)]
    paid_qa: bool,
    /// Maximum logical reader/judge requests, including resumed cache hits.
    #[arg(long)]
    max_requests: Option<u64>,
    /// Maximum total QA spend in USD, with at most six decimal places.
    #[arg(long)]
    max_usd: Option<String>,
}

#[derive(Debug, Clone, Args)]
struct RunArgs {
    /// Existing run directory.
    #[arg(long, value_name = "DIR")]
    output: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate(args) => validate_command(args),
        Command::Retrieve(args) => retrieve_command(args),
        Command::Qa(args) | Command::Resume(args) => qa_command(args),
        Command::Report(args) => report_command(args),
    }
}

fn validate_command(args: DatasetArgs) -> Result<()> {
    ensure_new_run(&args.output)?;
    let mut telemetry = RunTelemetry::new();
    let (dataset, validation, mut manifest) = initialize_run(&args)?;
    telemetry.sample_rss();
    let writer = ArtifactWriter::new(&args.output)?;
    writer.write_validation(&validation)?;
    if validation.has_fatal() {
        manifest.state = RunState::Blocked;
    }
    finish_telemetry(&mut manifest, &telemetry, 0, 0, 0);
    writer.write_report(&render_report(&manifest, &validation, &[]))?;
    writer.write_manifest(&manifest)?;
    writer.verify_checksums()?;
    if validation.has_fatal() {
        bail!("dataset validation failed with fatal findings");
    }
    ensure!(
        !dataset.questions.is_empty(),
        "validated dataset has no questions"
    );
    Ok(())
}

fn retrieve_command(args: RetrieveArgs) -> Result<()> {
    ensure!(args.dataset.seed_k > 0, "--seed-k must be positive");

    let mut telemetry = RunTelemetry::new();
    let (dataset, validation, mut manifest, fresh) = prepare_retrieval_run(&args.dataset)?;
    let writer = ArtifactWriter::new(&args.dataset.output)?;
    if fresh {
        writer.write_validation(&validation)?;
    } else {
        manifest.command = env::args().collect();
    }
    if validation.has_fatal() {
        manifest.state = RunState::Blocked;
        finish_telemetry(&mut manifest, &telemetry, 0, 0, 0);
        writer.write_report(&render_report(&manifest, &validation, &[]))?;
        writer.write_manifest(&manifest)?;
        bail!("dataset validation failed with fatal findings");
    }

    let traversal = traversal_config(&args.dataset);

    // Deliberately complete every variant in memory before publishing the
    // first ranking file. QA is a separate command and can only read these
    // frozen JSONL artifacts.
    let retrieval = rank_dataset(
        &dataset,
        retrieval_k(&args.dataset, dataset.kind)?,
        &traversal,
        &mut telemetry,
    )?;
    let metrics = score_retrieval(&dataset, &validation, &retrieval.rankings)?;
    for variant in VARIANTS {
        writer.write_rankings(&mut manifest, variant, &retrieval.rankings)?;
    }
    ensure!(
        manifest.ranking_hashes.len() == VARIANTS.len(),
        "retrieval did not freeze exactly five variant artifacts"
    );
    writer.write_metrics(&manifest, &metrics)?;
    verify_frozen_rankings(&args.dataset.output, &manifest)?;
    writer.verify_checksums()?;
    let persisted_metrics = read_metric_artifacts(&args.dataset.output, &manifest, Some(&metrics))?;
    manifest.gates = ReleaseGates::from_validation(
        &validation,
        retrieval_gate_evidence(
            retrieval.gold_leak_free,
            &persisted_metrics,
            retrieval_cohort(&validation, dataset.kind).len(),
        ),
    );
    manifest.transition(RunEvent::RetrievalComplete)?;
    manifest.transition(RunEvent::PublishRetrieval)?;
    manifest.transition(RunEvent::QaPending)?;

    finish_telemetry(&mut manifest, &telemetry, retrieval.index_bytes, 0, 0);
    writer.write_report(&render_report(&manifest, &validation, &persisted_metrics))?;
    writer.write_manifest(&manifest)?;
    writer.verify_checksums()?;
    Ok(())
}

fn qa_command(args: QaArgs) -> Result<()> {
    let paid = paid_authorization(&args)?;
    let mut manifest = read_json::<RunManifest>(&args.output.join("manifest.json"))?;
    ensure!(
        matches!(
            manifest.state,
            RunState::PublishedRetrieval | RunState::QaPending | RunState::PublishedFull
        ),
        "qa requires a published retrieval run"
    );
    let validation = read_json::<ValidationReport>(&args.output.join("validation.json"))?;
    let writer = ArtifactWriter::new(&args.output)?;
    writer.verify_recorded_checksums()?;
    verify_frozen_rankings(&args.output, &manifest)?;
    let dataset = load_manifest_dataset(&manifest)?;
    let rankings = read_frozen_rankings(&args.output)?;
    let validated_cache = validated_qa_cache_artifacts(&args.output, &dataset, &rankings)?;
    writer.reconcile_qa_cache_checksums(&validated_cache)?;
    let metrics = read_metric_artifacts(&args.output, &manifest, None)?;

    let Some(paid) = paid else {
        writer.write_report(&render_report(&manifest, &validation, &metrics))?;
        writer.write_manifest(&manifest)?;
        writer.verify_checksums()?;
        return Ok(());
    };
    ensure!(
        manifest.state != RunState::PublishedFull,
        "QA is already complete"
    );

    let mut telemetry = RunTelemetry::new();
    let completed = manifest.qa_progress.completed_question_ids().clone();
    let pending_rankings = rankings
        .into_iter()
        .filter(|((question_id, _, _), _)| !completed.contains(&query_id_string(question_id)))
        .collect::<RankingSet>();

    if pending_rankings.is_empty() {
        ensure!(
            manifest.qa_progress.completed_question_ids().len()
                == manifest.qa_progress.denominator(),
            "no pending rankings remain but the QA denominator is incomplete"
        );
        manifest.transition(RunEvent::QaComplete)?;
        manifest.transition(RunEvent::PublishFull)?;
        finish_telemetry(&mut manifest, &telemetry, 0, 0, 0);
        writer.write_report(&render_report(&manifest, &validation, &metrics))?;
        writer.write_manifest(&manifest)?;
        return Ok(());
    }

    let result = match dataset.kind {
        DatasetKind::Locomo => run_locomo_qa(
            &args.output,
            &dataset,
            &pending_rankings,
            &paid,
            &mut manifest,
            &writer,
        ),
        DatasetKind::LongMemEval => run_longmem_qa(
            &args.output,
            &dataset,
            &pending_rankings,
            &paid,
            &mut manifest,
            &writer,
        ),
    };

    let mut accounting = match result {
        Ok(accounting) => accounting,
        Err(error) => {
            let mut accounting = error
                .downcast_ref::<QaRunError>()
                .map(|failure| failure.accounting)
                .unwrap_or_default();
            accounting.merge_max(cache_accounting(&args.output.join("qa/cache"))?);
            telemetry.sample_rss();
            finish_telemetry(
                &mut manifest,
                &telemetry,
                0,
                accounting.context_tokens,
                accounting.requests,
            );
            record_max_hardware(
                &mut manifest,
                "qa_cost_usd_micros",
                accounting.cost_usd_micros,
            );
            writer.write_report(&render_report(&manifest, &validation, &metrics))?;
            // Refresh checksums after any cache records completed before the
            // API failure, without touching immutable ranking bytes.
            writer.write_manifest(&manifest)?;
            writer.verify_checksums()?;
            return Err(error);
        }
    };
    accounting.merge_max(cache_accounting(&args.output.join("qa/cache"))?);
    telemetry.sample_rss();

    if manifest.qa_progress.completed_question_ids().len() == manifest.qa_progress.denominator() {
        manifest.transition(RunEvent::QaComplete)?;
        manifest.transition(RunEvent::PublishFull)?;
    }
    finish_telemetry(
        &mut manifest,
        &telemetry,
        0,
        accounting.context_tokens,
        accounting.requests,
    );
    record_max_hardware(
        &mut manifest,
        "qa_cost_usd_micros",
        accounting.cost_usd_micros,
    );
    writer.write_report(&render_report(&manifest, &validation, &metrics))?;
    writer.write_manifest(&manifest)?;
    writer.verify_checksums()?;
    Ok(())
}

fn report_command(args: RunArgs) -> Result<()> {
    let manifest = read_json::<RunManifest>(&args.output.join("manifest.json"))?;
    let validation = read_json::<ValidationReport>(&args.output.join("validation.json"))?;
    let writer = ArtifactWriter::new(&args.output)?;
    verify_frozen_rankings_if_present(&args.output, &manifest)?;
    writer.verify_checksums()?;
    let metrics = read_metric_artifacts_if_present(&args.output, &manifest)?;
    writer.write_report(&render_report(&manifest, &validation, &metrics))?;
    writer.verify_checksums()?;
    Ok(())
}

fn initialize_run(args: &DatasetArgs) -> Result<(BenchmarkDataset, ValidationReport, RunManifest)> {
    let dataset = load_requested_dataset(args)?;
    let contract = benchmark_contract(args.track);
    let validation = validate_dataset(&dataset, &contract);
    let qa_question_ids = qa_question_ids(&dataset, &validation)?;
    let RepositoryInfo {
        revision,
        dirty,
        revision_kind,
    } = repository_info()?;
    let gates = ReleaseGates::from_validation(
        &validation,
        RetrievalGateEvidence {
            gold_leak_free: false,
            denominators_valid: false,
            metrics_finite: false,
        },
    );
    let mut manifest = RunManifest::new(
        run_id(&args.output),
        revision,
        dirty,
        vec![dataset.source.clone()],
        contract.contract_id,
        env::args().collect(),
        gates,
        QaProgress::new(qa_question_ids)?,
    );
    manifest.timestamps.insert(
        "started_at".to_owned(),
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    manifest.hardware.insert(
        "dataset_kind".to_owned(),
        dataset_name(dataset.kind).to_owned(),
    );
    manifest
        .hardware
        .insert("repository_revision_kind".to_owned(), revision_kind);
    manifest.hardware.insert(
        "source_path".to_owned(),
        requested_source_path(args)?.display().to_string(),
    );
    manifest.hardware.insert(
        "source_track".to_owned(),
        source_track_name(args.track).to_owned(),
    );
    record_variant_configuration(
        &mut manifest,
        retrieval_k(args, dataset.kind)?,
        &traversal_config(args),
    )?;
    Ok((dataset, validation, manifest))
}

fn prepare_retrieval_run(
    args: &DatasetArgs,
) -> Result<(BenchmarkDataset, ValidationReport, RunManifest, bool)> {
    let manifest_path = args.output.join("manifest.json");
    if !manifest_path.exists() {
        let (dataset, validation, manifest) = initialize_run(args)?;
        return Ok((dataset, validation, manifest, true));
    }

    let manifest = read_json::<RunManifest>(&manifest_path)?;
    ensure!(
        manifest.state == RunState::Validated,
        "retrieve cannot reuse terminal run in state {:?}",
        manifest.state
    );
    let writer = ArtifactWriter::new(&args.output)?;
    writer.verify_checksums()?;

    let stored_validation = read_json::<ValidationReport>(&args.output.join("validation.json"))?;
    let dataset = load_requested_dataset(args)?;
    let contract = benchmark_contract(args.track);
    let validation = validate_dataset(&dataset, &contract);
    ensure!(
        manifest.sources.as_slice() == [dataset.source.clone()],
        "source identity does not match validated run"
    );
    ensure!(
        manifest.contract_id == contract.contract_id,
        "contract does not match validated run"
    );
    ensure!(
        validation == stored_validation,
        "source validation does not match validated run"
    );

    let mut expected_configuration = manifest.clone();
    expected_configuration.variant_configuration.clear();
    expected_configuration.deterministic_seeds.clear();
    record_variant_configuration(
        &mut expected_configuration,
        retrieval_k(args, dataset.kind)?,
        &traversal_config(args),
    )?;
    ensure!(
        manifest.variant_configuration == expected_configuration.variant_configuration
            && manifest.deterministic_seeds == expected_configuration.deterministic_seeds,
        "retrieval configuration does not match validated run"
    );
    ensure!(
        manifest.qa_progress == QaProgress::new(qa_question_ids(&dataset, &validation)?)?,
        "QA denominator does not match validated run"
    );
    Ok((dataset, validation, manifest, false))
}

fn load_requested_dataset(args: &DatasetArgs) -> Result<BenchmarkDataset> {
    let (kind, path) = match (&args.locomo, &args.longmemeval) {
        (Some(path), None) => (DatasetKind::Locomo, path),
        (None, Some(path)) => (DatasetKind::LongMemEval, path),
        _ => bail!("exactly one of --locomo or --longmemeval is required"),
    };
    let path = fs::canonicalize(path).with_context(|| format!("resolve {}", path.display()))?;
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let actual_sha256 = sha256_hex(raw.as_bytes());
    let source = match args.track {
        SourceTrack::Audited => {
            let approved = approved_source(kind);
            let dataset = match kind {
                DatasetKind::Locomo => "LoCoMo",
                DatasetKind::LongMemEval => "LongMemEval-S-cleaned",
            };
            ensure!(
                actual_sha256 == approved.sha256,
                "source bytes do not match the approved {dataset} checksum"
            );
            if let Some(revision) = &args.source_revision {
                ensure!(
                    revision == &approved.revision,
                    "--source-revision does not match the approved {dataset} revision"
                );
            }
            approved
        }
        SourceTrack::Compatibility | SourceTrack::Smoke => {
            let revision = args
                .source_revision
                .clone()
                .unwrap_or_else(|| "local-input".to_owned());
            ensure!(
                !revision.trim().is_empty(),
                "--source-revision must not be empty"
            );
            SourcePin {
                origin: path.display().to_string(),
                revision,
                sha256: actual_sha256,
            }
        }
    };
    match kind {
        DatasetKind::Locomo => BenchmarkDataset::load_locomo(&raw, source),
        DatasetKind::LongMemEval => BenchmarkDataset::load_longmemeval(&raw, source),
    }
}

fn load_manifest_dataset(manifest: &RunManifest) -> Result<BenchmarkDataset> {
    let source = manifest
        .sources
        .first()
        .context("manifest has no dataset source")?
        .clone();
    let source_path = manifest
        .hardware
        .get("source_path")
        .context("manifest has no local source_path")?;
    let raw = fs::read_to_string(source_path)
        .with_context(|| format!("read manifest source {source_path}"))?;
    ensure!(
        sha256_hex(raw.as_bytes()) == source.sha256,
        "manifest source bytes changed since retrieval"
    );
    match manifest.hardware.get("dataset_kind").map(String::as_str) {
        Some("locomo") => BenchmarkDataset::load_locomo(&raw, source),
        Some("longmemeval") => BenchmarkDataset::load_longmemeval(&raw, source),
        other => bail!("manifest has unknown dataset_kind {other:?}"),
    }
}

fn requested_source_path(args: &DatasetArgs) -> Result<PathBuf> {
    let path = match (&args.locomo, &args.longmemeval) {
        (Some(path), None) | (None, Some(path)) => path,
        _ => bail!("exactly one of --locomo or --longmemeval is required"),
    };
    fs::canonicalize(path).with_context(|| format!("resolve {}", path.display()))
}

fn approved_source(kind: DatasetKind) -> SourcePin {
    let (origin, revision, sha256) = match kind {
        DatasetKind::Locomo => (LOCOMO_ORIGIN, LOCOMO_REVISION, LOCOMO_SHA256),
        DatasetKind::LongMemEval => (LONGMEMEVAL_ORIGIN, LONGMEMEVAL_REVISION, LONGMEMEVAL_SHA256),
    };
    SourcePin {
        origin: origin.to_owned(),
        revision: revision.to_owned(),
        sha256: sha256.to_owned(),
    }
}

fn benchmark_contract(track: SourceTrack) -> BenchmarkContract {
    match track {
        SourceTrack::Audited => BenchmarkContract::audited(CONTRACT_NAME),
        SourceTrack::Compatibility => {
            BenchmarkContract::compatibility(format!("{CONTRACT_NAME}-compatibility"))
        }
        SourceTrack::Smoke => BenchmarkContract::compatibility(format!("{CONTRACT_NAME}-smoke")),
    }
}

const fn source_track_name(track: SourceTrack) -> &'static str {
    match track {
        SourceTrack::Audited => "audited",
        SourceTrack::Compatibility => "compatibility",
        SourceTrack::Smoke => "smoke",
    }
}

fn traversal_config(args: &DatasetArgs) -> TraversalConfig {
    TraversalConfig {
        seed_k: args.seed_k,
        max_depth: args.max_depth,
        relations: BTreeSet::from([
            MemoryRelation::Contains,
            MemoryRelation::NextTurn,
            MemoryRelation::PreviousTurn,
            MemoryRelation::SpokenBy,
        ]),
    }
}

fn retrieval_k(args: &DatasetArgs, kind: DatasetKind) -> Result<usize> {
    let k = args.k.unwrap_or(match kind {
        DatasetKind::Locomo => LOCOMO_DEFAULT_K,
        DatasetKind::LongMemEval => LONGMEMEVAL_DEFAULT_K,
    });
    ensure!(k > 0, "--k must be positive");
    Ok(k)
}

fn qa_question_ids(
    dataset: &BenchmarkDataset,
    validation: &ValidationReport,
) -> Result<Vec<String>> {
    let ids = match dataset.kind {
        DatasetKind::Locomo => &validation.cohorts.locomo_qa,
        DatasetKind::LongMemEval => &validation.cohorts.longmemeval_qa,
    };
    if ids.is_empty() && validation.has_fatal() {
        let fallback = dataset
            .questions
            .iter()
            .map(|question| question.id.clone())
            .collect::<Vec<_>>();
        ensure!(!fallback.is_empty(), "dataset has no QA denominator");
        return Ok(fallback);
    }
    ensure!(!ids.is_empty(), "dataset has no QA denominator");
    Ok(ids.clone())
}

fn retrieval_cohort(validation: &ValidationReport, kind: DatasetKind) -> &[String] {
    match kind {
        DatasetKind::Locomo => &validation.cohorts.locomo_retrieval,
        DatasetKind::LongMemEval => &validation.cohorts.longmemeval_retrieval,
    }
}

fn qa_cohort(validation: &ValidationReport, kind: DatasetKind) -> &[String] {
    match kind {
        DatasetKind::Locomo => &validation.cohorts.locomo_qa,
        DatasetKind::LongMemEval => &validation.cohorts.longmemeval_qa,
    }
}

fn score_retrieval(
    dataset: &BenchmarkDataset,
    validation: &ValidationReport,
    rankings: &RankingSet,
) -> Result<Vec<RetrievalMetrics>> {
    let retrieval_ids = retrieval_cohort(validation, dataset.kind);
    let qa_ids = qa_cohort(validation, dataset.kind);
    ensure!(
        !retrieval_ids.is_empty(),
        "validated retrieval denominator must be positive"
    );
    let retrieval_id_set = retrieval_ids.iter().collect::<BTreeSet<_>>();
    let qa_id_set = qa_ids.iter().collect::<BTreeSet<_>>();
    ensure!(
        retrieval_id_set.len() == retrieval_ids.len()
            && qa_id_set.len() == qa_ids.len()
            && retrieval_id_set.is_subset(&qa_id_set),
        "validated retrieval cohort must be a unique subset of the full QA denominator"
    );
    let exclusions = qa_ids
        .iter()
        .filter(|question_id| !retrieval_id_set.contains(question_id))
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        retrieval_ids.len() + exclusions.len() == qa_ids.len(),
        "retrieval exclusions must preserve the full QA denominator"
    );

    let questions = dataset
        .questions
        .iter()
        .map(|question| (question.id.as_str(), question))
        .collect::<BTreeMap<_, _>>();
    let mut metrics = Vec::new();
    for &granularity in granularities(dataset.kind) {
        for variant in VARIANTS {
            let inputs = retrieval_ids
                .iter()
                .map(|question_id| {
                    let question = questions.get(question_id.as_str()).with_context(|| {
                        format!("retrieval cohort contains unknown {question_id}")
                    })?;
                    let ranking = rankings
                        .get(&(
                            QueryOccurrenceId::new(question_id.clone()),
                            variant,
                            granularity,
                        ))
                        .with_context(|| {
                            format!(
                                "missing ranking for {question_id}/{}/{variant:?}",
                                granularity_name(granularity)
                            )
                        })?
                        .clone();
                    Ok(RetrievalMetricInput {
                        question_id: question.id.clone(),
                        category: question.category,
                        question_type: question.question_type.clone(),
                        caption_evidence: question.gold_turn_ids.iter().any(|turn_id| {
                            dataset
                                .turn(turn_id)
                                .is_some_and(|turn| turn.caption.is_some())
                        }),
                        session_gold_ids: question.gold_session_ids.clone(),
                        turn_gold_ids: question.gold_turn_ids.clone(),
                        ranking,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let scored = match dataset.kind {
                DatasetKind::Locomo => score_locomo_retrieval(&inputs, exclusions.clone())?,
                DatasetKind::LongMemEval => {
                    score_longmemeval_retrieval(&inputs, exclusions.clone())?
                }
            };
            metrics.push(serde_json::from_value(serde_json::to_value(scored)?)?);
        }
    }
    validate_metric_collection(&metrics, dataset.kind, retrieval_ids.len(), &exclusions)?;
    Ok(metrics)
}

fn read_metric_artifacts(
    output: &Path,
    manifest: &RunManifest,
    expected: Option<&[RetrievalMetrics]>,
) -> Result<Vec<RetrievalMetrics>> {
    let kind = manifest_dataset_kind(manifest)?;
    let mut metrics = Vec::new();
    for &granularity in granularities(kind) {
        let path = output.join(format!(
            "metrics/{}-{}.json",
            dataset_name(kind),
            granularity_name(granularity)
        ));
        let artifact = read_json::<Value>(&path)
            .with_context(|| format!("read required metric artifact {}", path.display()))?;
        ensure!(
            artifact.get("dataset") == Some(&serde_json::to_value(kind)?)
                && artifact.get("granularity") == Some(&serde_json::to_value(granularity)?),
            "metric artifact identity does not match {}",
            path.display()
        );
        let variants = artifact
            .get("variants")
            .and_then(Value::as_array)
            .context("metric artifact variants must be an array")?;
        ensure!(
            variants.len() == VARIANTS.len(),
            "metric artifact must contain exactly five variants"
        );
        let mut seen = BTreeSet::new();
        for value in variants {
            let metric = serde_json::from_value::<RetrievalMetrics>(value.clone())?;
            ensure!(
                metric.dataset == kind && metric.granularity == granularity,
                "persisted metric disagrees with its dataset/granularity file"
            );
            ensure!(
                seen.insert(metric.variant),
                "metric artifact contains duplicate variant {:?}",
                metric.variant
            );
            let recorded_hash = manifest
                .ranking_hashes
                .get(&metric.variant)
                .context("metric variant has no manifest ranking hash")?;
            ensure!(
                value.get("source_ranking_hash").and_then(Value::as_str)
                    == Some(recorded_hash.as_str()),
                "metric source ranking hash does not match immutable ranking bytes"
            );
            metrics.push(metric);
        }
        ensure!(
            seen == VARIANTS.into_iter().collect(),
            "metric artifact does not cover every controlled variant"
        );
    }
    let exclusions = metrics
        .first()
        .context("metric evidence must not be empty")?
        .exclusions
        .clone();
    let qa_denominator = manifest.qa_progress.denominator();
    let retrieval_denominator = qa_denominator
        .checked_sub(exclusions.len())
        .context("metric exclusions exceed the full QA denominator")?;
    validate_metric_collection(&metrics, kind, retrieval_denominator, &exclusions)?;
    if let Some(expected) = expected {
        ensure!(
            metrics == expected,
            "persisted metric artifact content does not match computed metrics"
        );
    }
    Ok(metrics)
}

fn read_metric_artifacts_if_present(
    output: &Path,
    manifest: &RunManifest,
) -> Result<Vec<RetrievalMetrics>> {
    let has_metrics = fs::read_dir(output.join("metrics"))?.next().is_some();
    if !has_metrics {
        ensure!(
            matches!(manifest.state, RunState::Validated | RunState::Blocked),
            "published retrieval is missing metric evidence"
        );
        return Ok(Vec::new());
    }
    read_metric_artifacts(output, manifest, None)
}

fn manifest_dataset_kind(manifest: &RunManifest) -> Result<DatasetKind> {
    match manifest.hardware.get("dataset_kind").map(String::as_str) {
        Some("locomo") => Ok(DatasetKind::Locomo),
        Some("longmemeval") => Ok(DatasetKind::LongMemEval),
        value => bail!("manifest has invalid dataset kind {value:?}"),
    }
}

fn validate_metric_collection(
    metrics: &[RetrievalMetrics],
    kind: DatasetKind,
    retrieval_denominator: usize,
    exclusions: &[String],
) -> Result<()> {
    ensure!(
        retrieval_denominator > 0,
        "metric denominator must be positive"
    );
    ensure!(
        metrics.len() == granularities(kind).len() * VARIANTS.len(),
        "metric evidence has incorrect variant/granularity cardinality"
    );
    ensure!(
        exclusions.iter().all(|id| !id.is_empty())
            && exclusions.iter().collect::<BTreeSet<_>>().len() == exclusions.len(),
        "metric exclusions must be nonempty unique question IDs"
    );
    let mut aggregates = BTreeSet::new();
    for metric in metrics {
        ensure!(
            metric.dataset == kind
                && granularities(kind).contains(&metric.granularity)
                && metric.exclusions == exclusions,
            "metric aggregate identity or exclusions are invalid"
        );
        ensure!(
            aggregates.insert((metric.granularity, metric.variant)),
            "duplicate metric aggregate"
        );
        ensure!(
            !metric.overall.is_empty() && !metric.slices.is_empty(),
            "metric aggregate and slices must not be empty"
        );
        for value in metric.overall.values() {
            ensure!(
                metric_value_valid(value, retrieval_denominator, true),
                "overall metric has an invalid denominator or non-finite value"
            );
        }
        for slice in metric.slices.values() {
            ensure!(!slice.is_empty(), "metric slice must not be empty");
            for value in slice.values() {
                ensure!(
                    metric_value_valid(value, retrieval_denominator, false),
                    "slice metric has an invalid denominator or non-finite value"
                );
            }
        }
    }
    Ok(())
}

fn metric_value_valid(value: &MetricValue, expected: usize, exact: bool) -> bool {
    let denominator = usize::try_from(value.denominator).ok();
    let denominator_valid = denominator.is_some_and(|denominator| {
        denominator > 0 && denominator <= expected && (!exact || denominator == expected)
    });
    denominator_valid
        && value.numerator.is_finite()
        && value.value.is_finite()
        && (0.0..=value.denominator as f64).contains(&value.numerator)
        && (0.0..=1.0).contains(&value.value)
        && (value.value - value.numerator / value.denominator as f64).abs() <= 1e-12
}

fn retrieval_gate_evidence(
    gold_leak_free: bool,
    metrics: &[RetrievalMetrics],
    retrieval_denominator: usize,
) -> RetrievalGateEvidence {
    let denominators_valid = retrieval_denominator > 0
        && !metrics.is_empty()
        && metrics.iter().all(|metric| {
            metric
                .overall
                .values()
                .all(|value| metric_value_valid(value, retrieval_denominator, true))
                && metric.slices.values().all(|slice| {
                    slice
                        .values()
                        .all(|value| metric_value_valid(value, retrieval_denominator, false))
                })
        });
    let metrics_finite = !metrics.is_empty()
        && metrics.iter().all(|metric| {
            metric
                .overall
                .values()
                .chain(metric.slices.values().flat_map(|slice| slice.values()))
                .all(|value| value.value.is_finite() && value.numerator.is_finite())
        });
    RetrievalGateEvidence {
        gold_leak_free,
        denominators_valid,
        metrics_finite,
    }
}

struct RetrievalRun {
    rankings: RankingSet,
    index_bytes: u128,
    gold_leak_free: bool,
}

fn rank_dataset(
    dataset: &BenchmarkDataset,
    k: usize,
    traversal: &TraversalConfig,
    telemetry: &mut RunTelemetry,
) -> Result<RetrievalRun> {
    let mut rankings = RankingSet::new();
    let mut index_bytes = 0u128;
    for question in &dataset.questions {
        let sessions = memory_sessions(dataset, question)?;
        let graph = MemoryGraph::build(&sessions)?;
        let index_ranker = GraphIndexOnlyRanker::new(graph.clone())?;
        let traversal_ranker = GraphTraversalRanker::new(graph.clone(), traversal.clone())?;
        index_bytes = index_bytes
            .checked_add(index_ranker.build_telemetry().index_size_bytes as u128)
            .and_then(|value| {
                value.checked_add(traversal_ranker.build_telemetry().index_size_bytes as u128)
            })
            .context("index byte accounting overflow")?;

        for &granularity in granularities(dataset.kind) {
            let corpus = graph.corpus(granularity);
            let request = RankRequest {
                query: &question.text,
                granularity,
                corpus,
            };
            let gold = gold_ids(question, granularity);
            let oracle = oracle_ranking(
                &OracleRequest {
                    request: RankRequest {
                        query: request.query,
                        granularity,
                        corpus,
                    },
                    gold_occurrence_ids: gold,
                },
                k,
            );
            let recent = RecentRanker.rank(&request, k)?;
            let bm25 = Bm25Ranker::build(corpus.to_vec())?.rank(&request, k)?;
            let (index_only, index_query) = index_ranker.rank_with_telemetry(&request, k)?;
            let (graph_traversal, traversal_query) =
                traversal_ranker.rank_with_telemetry(&request, k)?;
            telemetry.query_nanoseconds = telemetry
                .query_nanoseconds
                .checked_add(index_query.query_nanoseconds)
                .and_then(|value| value.checked_add(traversal_query.query_nanoseconds))
                .context("query timing overflow")?;

            for ranking in [oracle, recent, bm25, index_only, graph_traversal] {
                let key = (
                    QueryOccurrenceId::new(question.id.clone()),
                    ranking.variant,
                    granularity,
                );
                ensure!(
                    rankings.insert(key, ranking).is_none(),
                    "duplicate ranking key for {}",
                    question.id
                );
            }
        }
        telemetry.sample_rss();
    }
    let expected = dataset
        .questions
        .len()
        .checked_mul(granularities(dataset.kind).len())
        .and_then(|value| value.checked_mul(VARIANTS.len()))
        .context("ranking denominator overflow")?;
    ensure!(
        rankings.len() == expected,
        "not every controlled ranking completed"
    );
    let expected_non_oracle = dataset
        .questions
        .len()
        .checked_mul(granularities(dataset.kind).len())
        .and_then(|value| value.checked_mul(VARIANTS.len() - 1))
        .context("non-oracle ranking denominator overflow")?;
    let gold_leak_free = rankings
        .iter()
        .filter(|((_, variant, _), _)| *variant != Variant::Oracle)
        .count()
        == expected_non_oracle
        && rankings.iter().all(|((_, variant, granularity), ranking)| {
            *variant == Variant::Oracle
                || (ranking.variant == *variant
                    && ranking.granularity == *granularity
                    && !ranking.query_sha256.is_empty()
                    && !ranking.corpus_sha256.is_empty()
                    && !ranking.serialization_sha256.is_empty())
        });
    ensure!(
        gold_leak_free,
        "typed non-oracle ranking contract did not cover every question/variant/granularity"
    );
    Ok(RetrievalRun {
        rankings,
        index_bytes,
        gold_leak_free,
    })
}

fn memory_sessions(
    dataset: &BenchmarkDataset,
    question: &QuestionRecord,
) -> Result<Vec<MemorySession>> {
    let conversation = question_conversation(dataset, question)?;
    conversation
        .sessions
        .iter()
        .enumerate()
        .map(|(session_index, session)| {
            let chronology = i64::try_from(session_index).context("session chronology overflow")?;
            let turns = session
                .turns
                .iter()
                .enumerate()
                .map(|(turn_index, turn)| {
                    let turn_offset =
                        i64::try_from(turn_index).context("turn chronology overflow")?;
                    let turn_chronology = chronology
                        .checked_mul(1_000_000)
                        .and_then(|value| value.checked_add(turn_offset))
                        .context("turn chronology overflow")?;
                    Ok(MemoryTurn {
                        document: CorpusDocument {
                            occurrence_id: turn.internal_id.clone(),
                            text: turn_text(turn),
                            chronology_key: Some(ChronologyKey::new(turn_chronology)),
                        },
                        speaker: turn.speaker.clone(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(MemorySession {
                document: CorpusDocument {
                    occurrence_id: session.internal_id.clone(),
                    text: session_text(session),
                    chronology_key: Some(ChronologyKey::new(chronology)),
                },
                turns,
            })
        })
        .collect()
}

fn question_conversation<'a>(
    dataset: &'a BenchmarkDataset,
    question: &QuestionRecord,
) -> Result<&'a spur_graph::memory_eval::contract::ConversationRecord> {
    let source_id = match dataset.kind {
        DatasetKind::Locomo => question
            .id
            .rsplit_once('#')
            .map_or(question.id.as_str(), |(source, _)| source),
        DatasetKind::LongMemEval => question.id.as_str(),
    };
    dataset
        .conversations
        .iter()
        .find(|conversation| conversation.source_id.as_deref() == Some(source_id))
        .with_context(|| format!("no canonical conversation for question {}", question.id))
}

fn turn_text(turn: &spur_graph::memory_eval::contract::TurnRecord) -> String {
    match turn.caption.as_deref() {
        Some(caption) => format!("{}\nCaption: {caption}", turn.content),
        None => turn.content.clone(),
    }
}

fn session_text(session: &SessionRecord) -> String {
    let mut text = String::new();
    if let Some(date) = &session.occurred_at {
        text.push_str("Date: ");
        text.push_str(date);
        text.push('\n');
    }
    for turn in &session.turns {
        if let Some(speaker) = &turn.speaker {
            text.push_str(speaker);
            text.push_str(": ");
        }
        text.push_str(&turn_text(turn));
        text.push('\n');
    }
    text
}

fn granularities(kind: DatasetKind) -> &'static [Granularity] {
    match kind {
        DatasetKind::Locomo => &[Granularity::Turn],
        DatasetKind::LongMemEval => &[Granularity::Session, Granularity::Turn],
    }
}

fn gold_ids(question: &QuestionRecord, granularity: Granularity) -> &[String] {
    match granularity {
        Granularity::Session => &question.gold_session_ids,
        Granularity::Turn => &question.gold_turn_ids,
    }
}

fn record_variant_configuration(
    manifest: &mut RunManifest,
    k: usize,
    traversal: &TraversalConfig,
) -> Result<()> {
    for variant in VARIANTS {
        let configuration = match variant {
            Variant::GraphTraversal => serde_json::to_value(traversal)?,
            _ => json!({"k": k}),
        };
        manifest
            .variant_configuration
            .insert(variant, configuration);
    }
    manifest
        .deterministic_seeds
        .insert("locomo_option_order".to_owned(), 0);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct PaidAuthorization {
    max_requests: u64,
    max_usd_micros: u64,
}

fn paid_authorization(args: &QaArgs) -> Result<Option<PaidAuthorization>> {
    if !args.paid_qa {
        ensure!(
            args.max_requests.is_none() && args.max_usd.is_none(),
            "--max-requests and --max-usd require --paid-qa"
        );
        return Ok(None);
    }
    let max_requests = args
        .max_requests
        .context("--paid-qa requires --max-requests")?;
    let max_usd = args
        .max_usd
        .as_deref()
        .context("--paid-qa requires --max-usd")?;
    ensure!(max_requests > 0, "--max-requests must be positive");
    let max_usd_micros = parse_usd_micros(max_usd)?;
    ensure!(max_usd_micros > 0, "--max-usd must be positive");
    ensure!(
        env::var("OPENAI_API_KEY")
            .ok()
            .is_some_and(|key| !key.trim().is_empty()),
        "--paid-qa requires nonempty OPENAI_API_KEY"
    );
    Ok(Some(PaidAuthorization {
        max_requests,
        max_usd_micros,
    }))
}

fn parse_usd_micros(value: &str) -> Result<u64> {
    ensure!(!value.starts_with('-'), "--max-usd must be nonnegative");
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.len() <= 6,
        "--max-usd must be a decimal with at most six fractional digits"
    );
    let whole = whole.parse::<u64>().context("--max-usd is too large")?;
    let mut padded = fraction.to_owned();
    padded.extend(std::iter::repeat_n('0', 6 - padded.len()));
    let fraction = if padded.is_empty() {
        0
    } else {
        padded.parse::<u64>().context("invalid --max-usd")?
    };
    whole
        .checked_mul(1_000_000)
        .and_then(|value| value.checked_add(fraction))
        .context("--max-usd is too large")
}

#[derive(Debug, Clone, Copy, Default)]
struct QaAccounting {
    context_tokens: u128,
    requests: u128,
    cost_usd_micros: u128,
}

#[derive(Debug)]
struct QaRunError {
    error: anyhow::Error,
    accounting: QaAccounting,
}

impl std::fmt::Display for QaRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.error)
    }
}

impl std::error::Error for QaRunError {}

impl QaAccounting {
    fn merge_max(&mut self, other: Self) {
        self.context_tokens = self.context_tokens.max(other.context_tokens);
        self.requests = self.requests.max(other.requests);
        self.cost_usd_micros = self.cost_usd_micros.max(other.cost_usd_micros);
    }
}

fn validated_qa_cache_artifacts(
    output: &Path,
    dataset: &BenchmarkDataset,
    rankings: &RankingSet,
) -> Result<Vec<ArtifactDigest>> {
    match dataset.kind {
        DatasetKind::Locomo => validated_locomo_cache_artifacts(output, dataset, rankings),
        DatasetKind::LongMemEval => JsonQaCache::open(output.join("qa/cache/longmemeval"))?
            .validated_artifacts_for_run(output, dataset, rankings),
    }
}

fn validated_locomo_cache_artifacts(
    output: &Path,
    dataset: &BenchmarkDataset,
    rankings: &RankingSet,
) -> Result<Vec<ArtifactDigest>> {
    let cache_root = output.join("qa/cache/locomo");
    if !cache_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries = BTreeMap::<String, (LocomoCacheEntry, ArtifactDigest)>::new();
    for directory_entry in fs::read_dir(&cache_root)
        .with_context(|| format!("read LoCoMo QA cache {}", cache_root.display()))?
    {
        let directory_entry = directory_entry?;
        let path = directory_entry.path();
        ensure!(
            directory_entry.file_type()?.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("json"),
            "unrecognized LoCoMo QA cache artifact {}",
            path.display()
        );
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let entry: LocomoCacheEntry = serde_json::from_slice(&bytes)
            .with_context(|| format!("decode LoCoMo QA cache {}", path.display()))?;
        let identity = sha256_hex(&serde_json::to_vec(&entry.request)?);
        ensure!(
            path.file_name().and_then(|name| name.to_str())
                == Some(format!("{identity}.json").as_str()),
            "LoCoMo QA cache filename does not match its complete request identity"
        );
        ensure!(
            entry.cost_usd_micros
                == token_cost_usd_micros(
                    entry.response.input_tokens,
                    entry.response.output_tokens,
                )?,
            "LoCoMo QA cache cost does not match its audited token usage"
        );
        let relative = path
            .strip_prefix(output)
            .with_context(|| format!("cache path {} is outside artifact root", path.display()))?
            .to_path_buf();
        let digest = ArtifactDigest {
            relative_path: relative,
            sha256: sha256_hex(&bytes),
        };
        ensure!(
            entries.insert(identity, (entry, digest)).is_none(),
            "duplicate LoCoMo QA cache identity"
        );
    }

    let mut recognized = BTreeSet::new();
    for ((question_id, variant, granularity), ranking) in rankings {
        let question = dataset
            .questions
            .iter()
            .find(|question| QueryOccurrenceId::new(question.id.clone()) == *question_id)
            .with_context(|| format!("ranking has unknown LoCoMo question {question_id:?}"))?;
        ensure!(
            ranking.variant == *variant && ranking.granularity == *granularity,
            "caller-owned ranking key disagrees with ranking payload"
        );
        let prompt = render_locomo_prompt_with_seed(question, ranking, dataset, 0)?;
        let request = QaRequest {
            question_id: question.id.clone(),
            variant: *variant,
            prompt_sha256: sha256_hex(prompt.as_bytes()),
            prompt,
            ranking_sha256: ranking_sha256(ranking)?,
            recorded_seed: 0,
        };
        let identity = sha256_hex(&serde_json::to_vec(&request)?);
        if let Some((entry, _)) = entries.get(&identity) {
            ensure!(
                entry.request == request,
                "LoCoMo QA cache identity mismatch"
            );
            recognized.insert(identity);
        }
    }
    ensure!(
        recognized.len() == entries.len(),
        "unrecognized LoCoMo QA cache record is outside the frozen workload"
    );

    Ok(entries
        .into_values()
        .map(|(_, artifact)| artifact)
        .collect())
}

fn cache_accounting(root: &Path) -> Result<QaAccounting> {
    if !root.is_dir() {
        return Ok(QaAccounting::default());
    }
    let mut accounting = QaAccounting::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read QA cache {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let value = read_json::<Value>(&path)?;
            let response = value
                .get("response")
                .and_then(Value::as_object)
                .context("QA cache entry has no response")?;
            let tokens = response
                .get("usage")
                .and_then(|usage| usage.get("total_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| {
                    response
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .zip(response.get("output_tokens").and_then(Value::as_u64))
                        .and_then(|(input, output)| input.checked_add(output))
                })
                .context("QA cache entry has invalid token usage")?;
            let cost = value
                .get("cost_usd_micros")
                .and_then(Value::as_u64)
                .context("QA cache entry has invalid cost")?;
            accounting.context_tokens = accounting
                .context_tokens
                .checked_add(u128::from(tokens))
                .context("QA cache token accounting overflow")?;
            accounting.cost_usd_micros = accounting
                .cost_usd_micros
                .checked_add(u128::from(cost))
                .context("QA cache cost accounting overflow")?;
            accounting.requests = accounting
                .requests
                .checked_add(1)
                .context("QA cache request accounting overflow")?;
        }
    }
    Ok(accounting)
}

fn run_longmem_qa(
    output: &Path,
    dataset: &BenchmarkDataset,
    rankings: &RankingSet,
    paid: &PaidAuthorization,
    manifest: &mut RunManifest,
    writer: &ArtifactWriter,
) -> Result<QaAccounting> {
    let api_key = env::var("OPENAI_API_KEY").context("read OPENAI_API_KEY")?;
    let mut backend = OpenAiResponsesBackend::new(Some(api_key));
    let mut cache = JsonQaCache::open(output.join("qa/cache/longmemeval"))?;
    let mut budget = QaBudget::new(QaBudgetLimits {
        max_requests: paid.max_requests,
        max_total_tokens: paid
            .max_requests
            .checked_mul(TOKENS_RESERVED_PER_REQUEST)
            .context("QA token ceiling overflow")?,
        max_usd_micros: paid.max_usd_micros,
        reserve_tokens_per_request: TOKENS_RESERVED_PER_REQUEST,
        reserve_usd_micros_per_request: 0,
        input_usd_micros_per_million: INPUT_USD_MICROS_PER_MILLION,
        output_usd_micros_per_million: OUTPUT_USD_MICROS_PER_MILLION,
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build QA runtime")?;
    let records = runtime.block_on(evaluate_longmem(
        dataset,
        rankings,
        &mut backend,
        &mut cache,
        &mut budget,
    ))?;
    persist_longmem_records(manifest, writer, &records)?;
    manifest.model = Some(LONGMEMEVAL_MODEL.to_owned());
    Ok(QaAccounting {
        context_tokens: u128::from(budget.usage().total_tokens),
        requests: u128::from(budget.requests()),
        cost_usd_micros: u128::from(budget.cost_usd_micros()),
    })
}

fn persist_longmem_records(
    manifest: &mut RunManifest,
    writer: &ArtifactWriter,
    records: &[LongMemQaRecord],
) -> Result<()> {
    let mut grouped = BTreeMap::<String, Vec<&LongMemQaRecord>>::new();
    for record in records {
        grouped
            .entry(record.question_id.clone())
            .or_default()
            .push(record);
        if let Some(hash) = &record.reader_prompt_sha256 {
            manifest.prompt_hashes.insert(
                format!(
                    "{}:{}:{}:reader",
                    record.question_id,
                    variant_name(record.variant),
                    granularity_name(record.granularity)
                ),
                hash.clone(),
            );
        }
        if let Some(hash) = &record.judge_prompt_sha256 {
            manifest.prompt_hashes.insert(
                format!(
                    "{}:{}:{}:judge",
                    record.question_id,
                    variant_name(record.variant),
                    granularity_name(record.granularity)
                ),
                hash.clone(),
            );
        }
    }
    for (question_id, group) in grouped {
        writer.write_qa_json(QaArtifactKind::Hypothesis, &question_id, &group)?;
        writer.write_qa_json(QaArtifactKind::JudgeInput, &question_id, &group)?;
        if group
            .iter()
            .all(|record| record.status == QaStatus::Complete)
        {
            writer.write_qa_label(manifest, &question_id, &group)?;
        }
    }
    Ok(())
}

fn run_locomo_qa(
    output: &Path,
    dataset: &BenchmarkDataset,
    rankings: &RankingSet,
    paid: &PaidAuthorization,
    manifest: &mut RunManifest,
    writer: &ArtifactWriter,
) -> Result<QaAccounting> {
    let mut backend = LocomoOpenAiBackend::open(output.join("qa/cache/locomo"), *paid)?;
    let records =
        evaluate_locomo(dataset, rankings, &mut backend, 0).map_err(|error| QaRunError {
            error,
            accounting: QaAccounting {
                context_tokens: backend.context_tokens,
                requests: u128::from(backend.requests),
                cost_usd_micros: u128::from(backend.cost_usd_micros),
            },
        })?;
    persist_locomo_records(manifest, writer, &records)?;
    manifest.model = Some(LONGMEMEVAL_MODEL.to_owned());
    Ok(QaAccounting {
        context_tokens: backend.context_tokens,
        requests: u128::from(backend.requests),
        cost_usd_micros: u128::from(backend.cost_usd_micros),
    })
}

fn persist_locomo_records(
    manifest: &mut RunManifest,
    writer: &ArtifactWriter,
    records: &[QaRecord],
) -> Result<()> {
    let mut grouped = BTreeMap::<String, Vec<&QaRecord>>::new();
    for record in records {
        manifest.prompt_hashes.insert(
            format!(
                "{}:{}:reader",
                record.question_id,
                variant_name(record.variant)
            ),
            record.prompt_sha256.clone(),
        );
        grouped
            .entry(record.question_id.clone())
            .or_default()
            .push(record);
    }
    for (question_id, group) in grouped {
        writer.write_qa_json(QaArtifactKind::Hypothesis, &question_id, &group)?;
        if group
            .iter()
            .all(|record| record.status == QaStatus::Complete)
        {
            writer.write_qa_label(manifest, &question_id, &group)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocomoCacheEntry {
    request: QaRequest,
    response: QaResponse,
    cost_usd_micros: u64,
}

struct LocomoOpenAiBackend {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
    api_key: String,
    cache_root: PathBuf,
    limits: PaidAuthorization,
    requests: u64,
    context_tokens: u128,
    cost_usd_micros: u64,
}

impl LocomoOpenAiBackend {
    fn open(cache_root: PathBuf, limits: PaidAuthorization) -> Result<Self> {
        fs::create_dir_all(&cache_root)
            .with_context(|| format!("create {}", cache_root.display()))?;
        Ok(Self {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build LoCoMo QA runtime")?,
            client: reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(120))
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(60))
                .build()
                .context("build LoCoMo QA client")?,
            api_key: env::var("OPENAI_API_KEY").context("read OPENAI_API_KEY")?,
            cache_root,
            limits,
            requests: 0,
            context_tokens: 0,
            cost_usd_micros: 0,
        })
    }

    fn admit(&mut self, _request: &QaRequest) -> Result<()> {
        let requests = self
            .requests
            .checked_add(1)
            .context("LoCoMo request count overflow")?;
        ensure!(
            requests <= self.limits.max_requests,
            "QA max request budget exhausted"
        );
        let reserve =
            token_cost_usd_micros(LONGMEMEVAL_MAX_INPUT_TOKENS, LOCOMO_MAX_OUTPUT_TOKENS)?;
        ensure!(
            self.cost_usd_micros
                .checked_add(reserve)
                .is_some_and(|cost| cost <= self.limits.max_usd_micros),
            "QA USD budget exhausted"
        );
        self.requests = requests;
        Ok(())
    }

    fn restore(&mut self, entry: &LocomoCacheEntry) -> Result<QaResponse> {
        let tokens = entry
            .response
            .input_tokens
            .checked_add(entry.response.output_tokens)
            .context("cached LoCoMo token overflow")?;
        self.context_tokens = self
            .context_tokens
            .checked_add(u128::from(tokens))
            .context("cached context-token accounting overflow")?;
        self.cost_usd_micros = self
            .cost_usd_micros
            .checked_add(entry.cost_usd_micros)
            .context("cached LoCoMo cost overflow")?;
        ensure!(
            self.cost_usd_micros <= self.limits.max_usd_micros,
            "cached QA USD budget exhausted"
        );
        Ok(entry.response.clone())
    }
}

impl QaBackend for LocomoOpenAiBackend {
    fn complete(&mut self, request: &QaRequest) -> Result<QaResponse> {
        self.admit(request)?;
        let identity = sha256_hex(&serde_json::to_vec(request)?);
        let path = self.cache_root.join(format!("{identity}.json"));
        if path.is_file() {
            let entry = read_json::<LocomoCacheEntry>(&path)?;
            ensure!(entry.request == *request, "LoCoMo cache identity mismatch");
            return self.restore(&entry);
        }

        let body = json!({
            "model": LONGMEMEVAL_MODEL,
            "input": request.prompt,
            "store": false,
            "temperature": 0,
            "max_output_tokens": LOCOMO_MAX_OUTPUT_TOKENS,
        });
        let response = self.runtime.block_on(async {
            let response = self
                .client
                .post(OPENAI_RESPONSES_URL)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .context("send LoCoMo OpenAI request")?;
            let status = response.status();
            let body = response
                .bytes()
                .await
                .context("read LoCoMo OpenAI response")?;
            ensure!(
                status.is_success(),
                "OpenAI Responses API HTTP status {status}"
            );
            let value: Value = serde_json::from_slice(&body).context("decode OpenAI response")?;
            ensure!(
                value.get("status").and_then(Value::as_str) == Some("completed"),
                "OpenAI response status is not completed"
            );
            let output_text = value
                .get("output_text")
                .and_then(Value::as_str)
                .context("OpenAI response has no string output_text")?
                .to_owned();
            let usage = value
                .get("usage")
                .and_then(Value::as_object)
                .context("OpenAI response has malformed usage")?;
            Ok::<_, anyhow::Error>(QaResponse {
                output_text,
                input_tokens: usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .context("OpenAI usage.input_tokens is malformed")?,
                output_tokens: usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .context("OpenAI usage.output_tokens is malformed")?,
            })
        })?;
        let cost_usd_micros = token_cost_usd_micros(response.input_tokens, response.output_tokens)?;
        let tokens = response
            .input_tokens
            .checked_add(response.output_tokens)
            .context("LoCoMo token accounting overflow")?;
        self.context_tokens = self
            .context_tokens
            .checked_add(u128::from(tokens))
            .context("LoCoMo context-token accounting overflow")?;
        self.cost_usd_micros = self
            .cost_usd_micros
            .checked_add(cost_usd_micros)
            .context("LoCoMo cost accounting overflow")?;
        ensure!(
            self.cost_usd_micros <= self.limits.max_usd_micros,
            "QA response exceeded the declared USD ceiling"
        );
        let entry = LocomoCacheEntry {
            request: request.clone(),
            response,
            cost_usd_micros,
        };
        let temporary = self.cache_root.join(format!(".{identity}.tmp"));
        fs::write(&temporary, serde_json::to_vec_pretty(&entry)?)?;
        fs::rename(&temporary, &path)?;
        Ok(entry.response)
    }
}

fn token_cost_usd_micros(input_tokens: u64, output_tokens: u64) -> Result<u64> {
    let numerator = u128::from(input_tokens)
        .checked_mul(u128::from(INPUT_USD_MICROS_PER_MILLION))
        .and_then(|value| {
            u128::from(output_tokens)
                .checked_mul(u128::from(OUTPUT_USD_MICROS_PER_MILLION))
                .and_then(|output| value.checked_add(output))
        })
        .context("QA cost overflow")?;
    u64::try_from(numerator.div_ceil(1_000_000)).context("QA cost overflow")
}

#[derive(Debug, Deserialize)]
struct PersistedRanking {
    question_id: QueryOccurrenceId,
    #[serde(flatten)]
    ranking: Ranking,
}

fn read_frozen_rankings(output: &Path) -> Result<RankingSet> {
    let mut rankings = RankingSet::new();
    for variant in VARIANTS {
        let path = output
            .join("rankings")
            .join(format!("{}.jsonl", variant_name(variant)));
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("read frozen ranking {}", path.display()))?;
        let mut count = 0usize;
        for (line_index, line) in contents.lines().enumerate() {
            let persisted: PersistedRanking = serde_json::from_str(line)
                .with_context(|| format!("decode {} line {}", path.display(), line_index + 1))?;
            ensure!(
                persisted.ranking.variant == variant,
                "ranking file variant disagrees with payload"
            );
            let key = (
                persisted.question_id,
                variant,
                persisted.ranking.granularity,
            );
            ensure!(
                rankings.insert(key, persisted.ranking).is_none(),
                "duplicate frozen ranking key"
            );
            count += 1;
        }
        ensure!(count > 0, "frozen ranking file is empty");
    }
    Ok(rankings)
}

fn verify_frozen_rankings(output: &Path, manifest: &RunManifest) -> Result<()> {
    ensure!(
        manifest.ranking_hashes.len() == VARIANTS.len(),
        "manifest does not contain exactly five frozen rankings"
    );
    for variant in VARIANTS {
        let expected = manifest
            .ranking_hashes
            .get(&variant)
            .with_context(|| format!("manifest omitted {:?} ranking", variant))?;
        let path = output
            .join("rankings")
            .join(format!("{}.jsonl", variant_name(variant)));
        let actual = sha256_hex(&fs::read(&path)?);
        ensure!(actual == *expected, "immutable ranking hash changed");
    }
    Ok(())
}

fn verify_frozen_rankings_if_present(output: &Path, manifest: &RunManifest) -> Result<()> {
    if manifest.ranking_hashes.is_empty() {
        return Ok(());
    }
    verify_frozen_rankings(output, manifest)
}

fn query_id_string(question_id: &QueryOccurrenceId) -> String {
    serde_json::to_value(question_id)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn render_report(
    manifest: &RunManifest,
    validation: &ValidationReport,
    metrics: &[RetrievalMetrics],
) -> String {
    let mut report = String::from("# Origin-faithful memory benchmark run\n\n");
    report.push_str(&format!("- Run: `{}`\n", manifest.run_id));
    report.push_str(&format!("- State: `{:?}`\n", manifest.state));
    report.push_str(&format!("- QA state: `{:?}`\n", manifest.qa_state));
    report.push_str(&format!(
        "- QA denominator: {} ({} complete)\n",
        manifest.qa_progress.denominator(),
        manifest.qa_progress.completed_question_ids().len()
    ));
    report.push_str(&format!(
        "- Repository: `{}` (dirty: {})\n",
        manifest.repository_revision, manifest.repository_dirty
    ));
    report.push_str(&format!(
        "- Frozen ranking variants: {}\n",
        manifest.ranking_hashes.len()
    ));
    report.push_str(&format!(
        "- Fatal validation findings: {}\n",
        validation.fatal.len()
    ));
    report.push_str("\n## Retrieval quality\n\n");
    if metrics.is_empty() {
        report.push_str("- Not computed for this validated run.\n");
    } else {
        for metric in metrics {
            report.push_str(&format!(
                "### {}/{}/{}\n\n",
                metric_dataset_name(metric.dataset),
                granularity_name(metric.granularity),
                variant_name(metric.variant)
            ));
            if let Some(hash) = manifest.ranking_hashes.get(&metric.variant) {
                report.push_str(&format!("- Source ranking SHA-256: `{hash}`\n"));
            }
            report.push_str(&format!(
                "- Scored denominator: {}\n",
                metric
                    .overall
                    .values()
                    .next()
                    .map_or(0, |value| value.denominator)
            ));
            report.push_str(&format!(
                "- Retrieval exclusions: {}\n",
                metric.exclusions.len()
            ));
            for (name, value) in &metric.overall {
                report.push_str(&format!(
                    "- {name}: {:.12} (numerator {:.12} / denominator {})\n",
                    value.value, value.numerator, value.denominator
                ));
            }
            for (slice_name, values) in &metric.slices {
                for (name, value) in values {
                    report.push_str(&format!(
                        "- slice {slice_name} / {name}: {:.12} (numerator {:.12} / denominator {})\n",
                        value.value, value.numerator, value.denominator
                    ));
                }
            }
            report.push('\n');
        }
    }
    report.push_str("\n## Telemetry\n\n");
    for key in [
        "duration_nanoseconds",
        "query_nanoseconds",
        "peak_rss_bytes",
        "index_bytes",
        "context_tokens",
        "qa_requests",
        "qa_cost_usd_micros",
    ] {
        if let Some(value) = manifest.hardware.get(key) {
            report.push_str(&format!("- {key}: {value}\n"));
        }
    }
    report
}

#[derive(Debug)]
struct RunTelemetry {
    started: Instant,
    peak_rss_bytes: u128,
    query_nanoseconds: u128,
}

impl RunTelemetry {
    fn new() -> Self {
        let mut telemetry = Self {
            started: Instant::now(),
            peak_rss_bytes: 0,
            query_nanoseconds: 0,
        };
        telemetry.sample_rss();
        telemetry
    }

    fn sample_rss(&mut self) {
        self.peak_rss_bytes = self.peak_rss_bytes.max(sample_rss_bytes());
    }
}

fn finish_telemetry(
    manifest: &mut RunManifest,
    telemetry: &RunTelemetry,
    index_bytes: u128,
    context_tokens: u128,
    qa_requests: u128,
) {
    manifest.timestamps.insert(
        "completed_at".to_owned(),
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    );
    record_sum_hardware(
        manifest,
        "duration_nanoseconds",
        telemetry.started.elapsed().as_nanos(),
    );
    record_sum_hardware(manifest, "query_nanoseconds", telemetry.query_nanoseconds);
    record_max_hardware(manifest, "peak_rss_bytes", telemetry.peak_rss_bytes);
    record_max_hardware(manifest, "index_bytes", index_bytes);
    record_max_hardware(manifest, "context_tokens", context_tokens);
    record_max_hardware(manifest, "qa_requests", qa_requests);
}

fn record_sum_hardware(manifest: &mut RunManifest, key: &str, value: u128) {
    let value = hardware_value(manifest, key)
        .checked_add(value)
        .unwrap_or(u128::MAX);
    manifest.hardware.insert(key.to_owned(), value.to_string());
}

fn record_max_hardware(manifest: &mut RunManifest, key: &str, value: u128) {
    let value = hardware_value(manifest, key).max(value);
    manifest.hardware.insert(key.to_owned(), value.to_string());
}

fn hardware_value(manifest: &RunManifest, key: &str) -> u128 {
    manifest
        .hardware
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn sample_rss_bytes() -> u128 {
    let pid = std::process::id().to_string();
    ProcessCommand::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|rss| rss.trim().parse::<u128>().ok())
        .and_then(|kibibytes| kibibytes.checked_mul(1024))
        .unwrap_or(0)
}

struct RepositoryInfo {
    revision: String,
    dirty: bool,
    revision_kind: String,
}

fn repository_info() -> Result<RepositoryInfo> {
    match git_output(["rev-parse", "HEAD"]) {
        Ok(revision) => {
            let dirty =
                !git_output(["status", "--porcelain", "--untracked-files=normal"])?.is_empty();
            Ok(RepositoryInfo {
                revision,
                dirty,
                revision_kind: "git_head".to_owned(),
            })
        }
        Err(_) => Ok(RepositoryInfo {
            revision: source_snapshot_revision()?,
            dirty: true,
            revision_kind: "scoped_source_sha256".to_owned(),
        }),
    }
}

fn source_snapshot_revision() -> Result<String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir.join("../..");
    let mut files = vec![workspace.join("Cargo.lock"), workspace.join("Cargo.toml")];
    collect_source_files(manifest_dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(&workspace).unwrap_or(&path);
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        hasher.update((relative.as_os_str().len() as u64).to_be_bytes());
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_source_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "target" {
                collect_source_files(&path, files)?;
            }
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn git_output<const N: usize>(arguments: [&str; N]) -> Result<String> {
    let output = ProcessCommand::new("git")
        .args(arguments)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .context("run git")?;
    ensure!(output.status.success(), "git command failed");
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn ensure_new_run(output: &Path) -> Result<()> {
    ensure!(
        !output.join("manifest.json").exists(),
        "run directory already contains a manifest; use qa, resume, or report"
    );
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("decode {}", path.display()))
}

fn run_id(output: &Path) -> String {
    output
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("memory-benchmark-run")
        .to_owned()
}

const fn dataset_name(kind: DatasetKind) -> &'static str {
    match kind {
        DatasetKind::Locomo => "locomo",
        DatasetKind::LongMemEval => "longmemeval",
    }
}

const fn metric_dataset_name(kind: DatasetKind) -> &'static str {
    match kind {
        DatasetKind::Locomo => "locomo",
        DatasetKind::LongMemEval => "long_mem_eval",
    }
}

const fn variant_name(variant: Variant) -> &'static str {
    match variant {
        Variant::Oracle => "oracle",
        Variant::Recent => "recent",
        Variant::FlatBm25 => "flat_bm25",
        Variant::GraphIndexOnly => "graph_index_only",
        Variant::GraphTraversal => "graph_traversal",
    }
}

const fn granularity_name(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Turn => "turn",
        Granularity::Session => "session",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod paid_boundary_tests {
    use super::*;
    use std::{
        io::Write,
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
        time::Instant,
    };

    fn backend(
        cache_root: PathBuf,
        limits: PaidAuthorization,
        client: reqwest::Client,
    ) -> LocomoOpenAiBackend {
        LocomoOpenAiBackend {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            client,
            api_key: "local-test-key".into(),
            cache_root,
            limits,
            requests: 0,
            context_tokens: 0,
            cost_usd_micros: 0,
        }
    }

    fn request(prompt: String, variant: Variant) -> QaRequest {
        QaRequest {
            question_id: "q-paid-boundary".into(),
            variant,
            prompt_sha256: sha256_hex(prompt.as_bytes()),
            prompt,
            ranking_sha256: "ranking-hash".into(),
            recorded_seed: 0,
        }
    }

    #[test]
    fn locomo_billed_request_overhead_is_rejected_without_a_physical_transmission() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let transmissions = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&transmissions);
        let proxy = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        observed.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(
                            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("mock proxy accept failed: {error}"),
                }
            }
        });
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{address}")).unwrap())
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mut backend = backend(
            temp.path().to_path_buf(),
            PaidAuthorization {
                max_requests: 1,
                // Exactly enough for the old one-byte visible prompt plus
                // the configured output cap, but not request framing.
                max_usd_micros: token_cost_usd_micros(1, LOCOMO_MAX_OUTPUT_TOKENS).unwrap(),
            },
            client,
        );

        let error = backend
            .complete(&request("x".into(), Variant::Oracle))
            .unwrap_err();
        proxy.join().unwrap();

        assert!(format!("{error:#}").contains("QA USD budget exhausted"));
        assert_eq!(transmissions.load(Ordering::SeqCst), 0);
        assert_eq!(backend.requests, 0);
        assert_eq!(backend.context_tokens, 0);
        assert_eq!(backend.cost_usd_micros, 0);
    }

    #[test]
    fn locomo_cached_success_then_request_rejection_keeps_monotonic_accounting() {
        let temp = tempfile::tempdir().unwrap();
        let first = request("cached prompt".into(), Variant::Oracle);
        let entry = LocomoCacheEntry {
            request: first.clone(),
            response: QaResponse {
                output_text: "blue".into(),
                input_tokens: 10,
                output_tokens: 1,
            },
            cost_usd_micros: 35,
        };
        let identity = sha256_hex(&serde_json::to_vec(&first).unwrap());
        fs::write(
            temp.path().join(format!("{identity}.json")),
            serde_json::to_vec_pretty(&entry).unwrap(),
        )
        .unwrap();
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let mut backend = backend(
            temp.path().to_path_buf(),
            PaidAuthorization {
                max_requests: 1,
                max_usd_micros: 1_000_000,
            },
            client,
        );

        let response = backend.complete(&first).unwrap();
        assert_eq!(response.output_text, "blue");
        let error = backend
            .complete(&request("rejected prompt".into(), Variant::Recent))
            .unwrap_err();

        assert!(format!("{error:#}").contains("QA max request budget exhausted"));
        assert_eq!(backend.requests, 1);
        assert_eq!(backend.context_tokens, 11);
        assert_eq!(backend.cost_usd_micros, 35);
    }
}
