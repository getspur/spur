use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

const LOCOMO_FIXTURE: &str = r#"
[
  {
    "sample_id": "conv-runner",
    "conversation": {
      "speaker_a": "Alice",
      "speaker_b": "Bob",
      "session_1_date_time": "2023-01-01",
      "session_1": [
        {"speaker": "Bob", "dia_id": "D1:1", "text": "Earlier context."},
        {"speaker": "Alice", "dia_id": "D1:2", "text": "The answer is blue."}
      ]
    },
    "qa": [
      {
        "question": "What color was the answer?",
        "answer": "blue",
        "category": 1,
        "evidence": ["D1:2"]
      },
      {
        "question": "Did Alice visit Mars?",
        "category": 5,
        "evidence": [],
        "adversarial_answer": "Yes, Alice visited Mars"
      }
    ]
  }
]
"#;

const LONGMEMEVAL_FIXTURE: &str = r#"
[
  {
    "question_id": "q-retrieval",
    "question_type": "multi-session",
    "question": "What travel choice was made?",
    "answer": "Take the train",
    "question_date": "2024-01-03",
    "haystack_session_ids": ["earlier", "answer"],
    "haystack_dates": ["2024-01-01", "2024-01-02"],
    "haystack_sessions": [
      [{"role": "user", "content": "I need to travel."}],
      [{"role": "assistant", "content": "Take the train.", "has_answer": true}]
    ],
    "answer_session_ids": ["answer"]
  },
  {
    "question_id": "q-abstention_abs",
    "question_type": "single-session-user",
    "question": "Was a destination selected?",
    "answer": "No",
    "question_date": "2024-01-03",
    "haystack_session_ids": ["abstention"],
    "haystack_dates": ["2024-01-01"],
    "haystack_sessions": [
      [{"role": "user", "content": "No destination was selected."}]
    ],
    "answer_session_ids": []
  }
]
"#;

const VARIANTS: [&str; 5] = [
    "oracle",
    "recent",
    "flat_bm25",
    "graph_index_only",
    "graph_traversal",
];

#[test]
fn legal_subcommands_and_validate_lifecycle_are_available() {
    let help = Command::new(binary()).arg("--help").output().unwrap();
    assert!(help.status.success(), "{}", stderr(&help));
    let help = String::from_utf8(help.stdout).unwrap();
    for subcommand in ["validate", "retrieve", "qa", "resume", "report"] {
        assert!(help.contains(subcommand), "missing {subcommand}: {help}");
    }

    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    let output = run_cli([
        "validate",
        "--locomo",
        fixture.path().to_str().unwrap(),
        "--output",
        run.path().to_str().unwrap(),
        "--track",
        "smoke",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = read_json(run.path().join("manifest.json"));
    assert_eq!(manifest["state"], "validated");
}

#[test]
fn local_fixture_is_explicitly_smoke_and_never_audited() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    let output = run_cli([
        "validate",
        "--locomo",
        fixture.path().to_str().unwrap(),
        "--output",
        run.path().to_str().unwrap(),
        "--track",
        "smoke",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = read_json(run.path().join("manifest.json"));
    assert_eq!(manifest["contract_id"]["track"], "compatibility");
    assert_eq!(manifest["contract_id"]["name"], "origin-faithful-v1-smoke");
    assert_ne!(manifest["contract_id"]["track"], "audited");
}

#[test]
fn modified_or_unknown_source_cannot_publish_as_audited_run() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    let output = run_cli([
        "retrieve",
        "--locomo",
        fixture.path().to_str().unwrap(),
        "--output",
        run.path().to_str().unwrap(),
        "--track",
        "audited",
    ]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("approved LoCoMo checksum"));
    assert!(!run.path().join("manifest.json").exists());
    assert!(!run.path().join("rankings").exists());
}

#[test]
fn matching_validate_then_retrieve_continues_in_the_same_directory() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    let validate = validate(&fixture, run.path(), "smoke");
    assert!(validate.status.success(), "{}", stderr(&validate));

    let retrieve = retrieve(&fixture, run.path());
    assert!(retrieve.status.success(), "{}", stderr(&retrieve));
    let manifest = read_json(run.path().join("manifest.json"));
    assert_eq!(manifest["state"], "published_retrieval");
    assert_eq!(manifest["contract_id"]["track"], "compatibility");
}

#[test]
fn mismatched_validate_continuations_fail_without_mutation() {
    let fixture = fixture();

    let config_run = tempfile::tempdir().unwrap();
    assert!(validate(&fixture, config_run.path(), "smoke")
        .status
        .success());
    assert_rejected_without_mutation(config_run.path(), || {
        run_cli([
            "retrieve",
            "--locomo",
            fixture.path().to_str().unwrap(),
            "--output",
            config_run.path().to_str().unwrap(),
            "--track",
            "smoke",
            "--k",
            "9",
        ])
    });

    let contract_run = tempfile::tempdir().unwrap();
    assert!(validate(&fixture, contract_run.path(), "smoke")
        .status
        .success());
    assert_rejected_without_mutation(contract_run.path(), || {
        run_cli([
            "retrieve",
            "--locomo",
            fixture.path().to_str().unwrap(),
            "--output",
            contract_run.path().to_str().unwrap(),
            "--track",
            "compatibility",
        ])
    });

    let source = tempfile::NamedTempFile::new().unwrap();
    fs::write(source.path(), LOCOMO_FIXTURE).unwrap();
    let source_run = tempfile::tempdir().unwrap();
    assert!(validate(&source, source_run.path(), "smoke")
        .status
        .success());
    fs::write(source.path(), format!("{LOCOMO_FIXTURE}\n ")).unwrap();
    assert_rejected_without_mutation(source_run.path(), || {
        run_cli([
            "retrieve",
            "--locomo",
            source.path().to_str().unwrap(),
            "--output",
            source_run.path().to_str().unwrap(),
            "--track",
            "smoke",
        ])
    });
}

#[test]
fn terminal_retrieval_reuse_fails_without_mutation() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    assert!(retrieve(&fixture, run.path()).status.success());
    assert_rejected_without_mutation(run.path(), || retrieve(&fixture, run.path()));
}

#[test]
fn cargo_lock_records_the_runner_clap_dependency() {
    let lock =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock")).unwrap();
    let package = lock
        .split("[[package]]")
        .find(|package| package.contains("name = \"spur-graph\""))
        .expect("spur-graph package must be present in Cargo.lock");
    assert!(
        package.lines().any(|line| line.trim() == "\"clap\","),
        "fresh builds must not need to repair the spur-graph dependency edge"
    );
}

#[test]
fn retrieval_only_run_publishes_five_frozen_variants_and_complete_qa_pending() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    let output = retrieve(&fixture, run.path());
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = read_json(run.path().join("manifest.json"));
    assert_eq!(manifest["state"], "published_retrieval");
    assert_eq!(manifest["qa_state"], "qa_pending");
    assert_eq!(
        manifest["qa_progress"]["eligible_question_ids"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(manifest["ranking_hashes"].as_object().unwrap().len(), 5);

    assert_metric_file(
        run.path(),
        "locomo-turn.json",
        "locomo",
        "turn",
        &[
            "all_evidence_hit_at_1",
            "all_evidence_hit_at_10",
            "all_evidence_hit_at_5",
            "evidence_recall_at_1",
            "evidence_recall_at_10",
            "evidence_recall_at_5",
        ],
        1,
        1,
    );
    assert_eq!(
        fs::read_dir(run.path().join("metrics")).unwrap().count(),
        1,
        "published LoCoMo retrieval must contain exactly one metric artifact"
    );

    for variant in VARIANTS {
        let ranking = run.path().join(format!("rankings/{variant}.jsonl"));
        let lines = fs::read_to_string(&ranking).unwrap();
        assert_eq!(lines.lines().count(), 2, "{}", ranking.display());
    }
    assert!(fs::read_dir(run.path().join("qa/labels"))
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn release_gates_remain_false_until_nonempty_metrics_are_computed() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    let output = validate(&fixture, run.path(), "smoke");
    assert!(output.status.success(), "{}", stderr(&output));

    let validated = read_json(run.path().join("manifest.json"));
    assert_eq!(validated["state"], "validated");
    assert_eq!(validated["gates"]["denominators_valid"], false);
    assert_eq!(validated["gates"]["metrics_finite"], false);
    assert!(fs::read_dir(run.path().join("metrics"))
        .unwrap()
        .next()
        .is_none());

    let output = retrieve(&fixture, run.path());
    assert!(output.status.success(), "{}", stderr(&output));
    let published = read_json(run.path().join("manifest.json"));
    assert_eq!(published["state"], "published_retrieval");
    assert_eq!(published["gates"]["denominators_valid"], true);
    assert_eq!(published["gates"]["metrics_finite"], true);
    assert!(fs::read_dir(run.path().join("metrics"))
        .unwrap()
        .next()
        .is_some());
}

#[test]
fn longmemeval_publishes_session_and_turn_metrics_for_only_the_validated_cohort() {
    let fixture = longmemeval_fixture();
    let run = tempfile::tempdir().unwrap();
    let output = retrieve_longmemeval(&fixture, run.path());
    assert!(output.status.success(), "{}", stderr(&output));

    let manifest = read_json(run.path().join("manifest.json"));
    assert_eq!(manifest["state"], "published_retrieval");
    assert_eq!(
        manifest["qa_progress"]["eligible_question_ids"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "the full native QA denominator must be retained"
    );

    assert_metric_file(
        run.path(),
        "longmemeval-session.json",
        "long_mem_eval",
        "session",
        &[
            "ndcg_any@10",
            "ndcg_any@5",
            "recall_all@10",
            "recall_all@5",
            "recall_any@10",
            "recall_any@5",
        ],
        1,
        1,
    );
    assert_metric_file(
        run.path(),
        "longmemeval-turn.json",
        "long_mem_eval",
        "turn",
        &[
            "ndcg_any@10",
            "ndcg_any@5",
            "ndcg_any@50",
            "recall_all@10",
            "recall_all@5",
            "recall_all@50",
            "recall_any@10",
            "recall_any@5",
            "recall_any@50",
        ],
        1,
        1,
    );
    assert_eq!(
        fs::read_dir(run.path().join("metrics")).unwrap().count(),
        2,
        "published LongMemEval retrieval must contain both native granularities"
    );

    let report = fs::read_to_string(run.path().join("report.md")).unwrap();
    assert!(report.contains("## Retrieval quality"), "{report}");
    assert!(report.contains("long_mem_eval/session/oracle"), "{report}");
    assert!(
        report.contains("long_mem_eval/turn/graph_traversal"),
        "{report}"
    );
    assert!(report.contains("## Telemetry"), "{report}");
}

#[test]
fn qa_without_paid_authorization_is_a_monotonic_noop() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    assert!(retrieve(&fixture, run.path()).status.success());
    let before = ranking_bytes(run.path());

    let output = run_cli(["qa", "--output", run.path().to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(ranking_bytes(run.path()), before);
    let manifest = read_json(run.path().join("manifest.json"));
    assert_eq!(manifest["state"], "published_retrieval");
    assert_eq!(manifest["qa_state"], "qa_pending");
}

#[test]
fn paid_qa_requires_flag_both_bounds_and_openai_key_without_mutating_rankings() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    assert!(retrieve(&fixture, run.path()).status.success());
    let before = ranking_bytes(run.path());

    let missing_usd = run_cli_without_key([
        "qa",
        "--output",
        run.path().to_str().unwrap(),
        "--paid-qa",
        "--max-requests",
        "10",
    ]);
    assert!(!missing_usd.status.success());
    assert!(stderr(&missing_usd).contains("--max-usd"));

    let missing_key = run_cli_without_key([
        "resume",
        "--output",
        run.path().to_str().unwrap(),
        "--paid-qa",
        "--max-requests",
        "10",
        "--max-usd",
        "1.00",
    ]);
    assert!(!missing_key.status.success());
    assert!(stderr(&missing_key).contains("OPENAI_API_KEY"));
    assert_eq!(ranking_bytes(run.path()), before);

    let bounds_without_paid = run_cli_without_key([
        "qa",
        "--output",
        run.path().to_str().unwrap(),
        "--max-requests",
        "10",
        "--max-usd",
        "1.00",
    ]);
    assert!(!bounds_without_paid.status.success());
    assert!(stderr(&bounds_without_paid).contains("--paid-qa"));
}

#[test]
fn report_records_reproducibility_and_complete_nonnegative_accounting() {
    let fixture = fixture();
    let run = tempfile::tempdir().unwrap();
    assert!(retrieve(&fixture, run.path()).status.success());

    let output = run_cli(["report", "--output", run.path().to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = read_json(run.path().join("manifest.json"));
    assert!(manifest["repository_revision"].as_str().unwrap().len() >= 40);
    assert!(manifest["repository_dirty"].is_boolean());
    assert!(manifest["command"]
        .as_array()
        .unwrap()
        .iter()
        .any(|argument| argument == "retrieve"));

    let hardware = manifest["hardware"].as_object().unwrap();
    for field in [
        "duration_nanoseconds",
        "peak_rss_bytes",
        "index_bytes",
        "context_tokens",
    ] {
        let _: u128 = hardware[field].as_str().unwrap().parse().unwrap();
    }
    assert!(
        hardware["index_bytes"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap()
            > 0
    );
    assert!(run.path().join("report.md").is_file());
}

fn fixture() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), LOCOMO_FIXTURE).unwrap();
    file
}

fn longmemeval_fixture() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), LONGMEMEVAL_FIXTURE).unwrap();
    file
}

fn retrieve(fixture: &tempfile::NamedTempFile, run: &Path) -> std::process::Output {
    run_cli([
        "retrieve",
        "--locomo",
        fixture.path().to_str().unwrap(),
        "--output",
        run.to_str().unwrap(),
        "--track",
        "smoke",
    ])
}

fn retrieve_longmemeval(fixture: &tempfile::NamedTempFile, run: &Path) -> std::process::Output {
    run_cli([
        "retrieve",
        "--longmemeval",
        fixture.path().to_str().unwrap(),
        "--output",
        run.to_str().unwrap(),
        "--track",
        "smoke",
    ])
}

fn validate(fixture: &tempfile::NamedTempFile, run: &Path, track: &str) -> std::process::Output {
    run_cli([
        "validate",
        "--locomo",
        fixture.path().to_str().unwrap(),
        "--output",
        run.to_str().unwrap(),
        "--track",
        track,
    ])
}

fn run_cli<const N: usize>(arguments: [&str; N]) -> std::process::Output {
    Command::new(binary()).args(arguments).output().unwrap()
}

fn run_cli_without_key<const N: usize>(arguments: [&str; N]) -> std::process::Output {
    Command::new(binary())
        .args(arguments)
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap()
}

fn binary() -> String {
    std::env::var("CARGO_BIN_EXE_memory_benchmark")
        .expect("memory_benchmark binary must be built for the runner test")
}

fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn assert_metric_file(
    run: &Path,
    file_name: &str,
    dataset: &str,
    granularity: &str,
    expected_metric_names: &[&str],
    expected_exclusions: usize,
    expected_denominator: u64,
) {
    let manifest = read_json(run.join("manifest.json"));
    let metric_file = read_json(run.join("metrics").join(file_name));
    assert_eq!(metric_file["dataset"], dataset);
    assert_eq!(metric_file["granularity"], granularity);
    let variants = metric_file["variants"].as_array().unwrap();
    assert_eq!(variants.len(), VARIANTS.len());

    let mut actual_variants = variants
        .iter()
        .map(|metric| metric["variant"].as_str().unwrap())
        .collect::<Vec<_>>();
    actual_variants.sort_unstable();
    let mut expected_variants = VARIANTS.to_vec();
    expected_variants.sort_unstable();
    assert_eq!(actual_variants, expected_variants);

    for metric in variants {
        let variant = metric["variant"].as_str().unwrap();
        assert_eq!(metric["dataset"], dataset);
        assert_eq!(metric["granularity"], granularity);
        assert_eq!(
            metric["source_ranking_hash"], manifest["ranking_hashes"][variant],
            "{dataset}/{granularity}/{variant} must bind the exact ranking bytes"
        );
        let ranking_bytes = fs::read(run.join(format!("rankings/{variant}.jsonl"))).unwrap();
        let ranking_sha256 = format!("{:x}", Sha256::digest(&ranking_bytes));
        assert_eq!(metric["source_ranking_hash"], ranking_sha256);
        let exclusions = metric["exclusions"].as_array().unwrap();
        assert_eq!(exclusions.len(), expected_exclusions);
        let eligible = manifest["qa_progress"]["eligible_question_ids"]
            .as_array()
            .unwrap();
        assert_eq!(
            expected_denominator as usize + exclusions.len(),
            eligible.len(),
            "scored and excluded cohorts must preserve the full QA denominator"
        );
        assert!(exclusions.iter().all(|id| eligible.contains(id)));
        let overall = metric["overall"].as_object().unwrap();
        assert_eq!(
            overall.keys().map(String::as_str).collect::<Vec<_>>(),
            expected_metric_names
        );
        assert!(!metric["slices"].as_object().unwrap().is_empty());
        for values in std::iter::once(overall).chain(
            metric["slices"]
                .as_object()
                .unwrap()
                .values()
                .map(|slice| slice.as_object().unwrap()),
        ) {
            for value in values.values() {
                let score = value["value"].as_f64().unwrap();
                let numerator = value["numerator"].as_f64().unwrap();
                let denominator = value["denominator"].as_u64().unwrap();
                assert!(score.is_finite() && (0.0..=1.0).contains(&score));
                assert!(numerator.is_finite() && numerator >= 0.0);
                assert_eq!(denominator, expected_denominator);
                assert!((score - numerator / denominator as f64).abs() < 1e-12);
            }
        }
    }
}

fn ranking_bytes(run: &Path) -> BTreeMap<String, Vec<u8>> {
    VARIANTS
        .into_iter()
        .map(|variant| {
            let path = run.join(format!("rankings/{variant}.jsonl"));
            (variant.to_owned(), fs::read(path).unwrap())
        })
        .collect()
}

fn directory_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = fs::read_dir(current)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

fn assert_rejected_without_mutation(run: &Path, command: impl FnOnce() -> std::process::Output) {
    let before = directory_bytes(run);
    let output = command();
    assert!(!output.status.success(), "command unexpectedly succeeded");
    assert!(
        stderr(&output).contains("does not match validated run")
            || stderr(&output).contains("terminal run"),
        "unexpected rejection: {}",
        stderr(&output)
    );
    assert_eq!(directory_bytes(run), before);
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
