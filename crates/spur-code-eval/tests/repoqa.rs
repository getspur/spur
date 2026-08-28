use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub use spur_code_eval::{
    CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldEvidence, Language, QueryPolicy,
    RepositoryPin, SourceFormat, SourceIdentity, SourceSpec, Suite,
};

#[path = "../src/repoqa.rs"]
mod repoqa;

use repoqa::{
    RepoQaAdapter, RepoQaModelScoreInput, RepoQaRecord, RepoQaSourceSymbol, RepoQaTranslation,
};
use serde_json::{json, Value};

const FIXTURE: &[u8] = include_bytes!("fixtures/repoqa.json");
const MANIFEST: &str = include_str!("../benchmarks/code_eval.toml");
const RUST_BUILD_CASE: &str = "rust::synthetic/repo::build_answer";

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct FixtureRepository {
    base: PathBuf,
    root: PathBuf,
}

impl FixtureRepository {
    fn new() -> Self {
        let unique = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "spur-code-eval-repoqa-{}-{unique}",
            std::process::id()
        ));
        let root = base.join("repository");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("web")).unwrap();
        fs::create_dir_all(root.join("java")).unwrap();
        fs::write(
            root.join("src/build-answer.rs"),
            "pub fn build_answer(value: usize) -> usize {\n    value + 1\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/format.value.rs"),
            "pub fn format_value(value: usize) -> String {\n    format!(\"value={value}\")\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("web/render-widget.ts"),
            "export function renderWidget(name: string): string {\n  return `<${name}>`;\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("java/parse.item.java"),
            "static int parseItem(int value) {\n  return value;\n}\n",
        )
        .unwrap();
        Self { base, root }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn outside_path(&self) -> PathBuf {
        self.base.join("outside.rs")
    }
}

impl Drop for FixtureRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn adapter() -> RepoQaAdapter {
    let manifest = spur_code_eval::SourceManifest::from_toml(MANIFEST).unwrap();
    let source = manifest
        .sources()
        .iter()
        .find(|source| source.suite() == Suite::RepoQa)
        .unwrap();
    RepoQaAdapter::new(source).unwrap()
}

fn fixture_records() -> Vec<RepoQaRecord> {
    serde_json::from_slice(FIXTURE).unwrap()
}

fn fixture_values() -> Vec<Value> {
    serde_json::from_slice(FIXTURE).unwrap()
}

fn records_with(first_record_patch: impl FnOnce(&mut Value)) -> Vec<RepoQaRecord> {
    let mut values = fixture_values();
    first_record_patch(&mut values[0]);
    serde_json::from_value(Value::Array(values)).unwrap()
}

fn source_symbols(
    repository: &FixtureRepository,
    records: &[RepoQaRecord],
) -> Vec<RepoQaSourceSymbol> {
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let source = fs::read(repository.root().join(record.path())).unwrap();
            let (byte_start, byte_end) =
                line_span(&source, record.start_line(), record.end_line()).unwrap();
            let identity = SourceIdentity::new(
                record.path(),
                byte_start,
                byte_end,
                Some(format!("spur-symbol-{index}")),
            )
            .unwrap();
            RepoQaSourceSymbol::new(record.name(), identity).unwrap()
        })
        .collect()
}

fn line_span(source: &[u8], start_line: usize, end_line: usize) -> Option<(u64, u64)> {
    if start_line >= end_line {
        return None;
    }
    let mut starts = vec![0_usize];
    for (index, byte) in source.iter().enumerate() {
        if *byte == b'\n' {
            starts.push(index + 1);
        }
    }
    if end_line > starts.len() {
        return None;
    }
    let start = *starts.get(start_line)?;
    let end = starts.get(end_line).copied().unwrap_or(source.len());
    Some((u64::try_from(start).ok()?, u64::try_from(end).ok()?))
}

fn translate_fixture(
    repository: &FixtureRepository,
    records: &[RepoQaRecord],
    symbols: &[RepoQaSourceSymbol],
) -> Vec<RepoQaTranslation> {
    adapter()
        .translate(records, repository.root(), symbols)
        .unwrap()
}

fn translation<'a>(translations: &'a [RepoQaTranslation], case_id: &str) -> &'a RepoQaTranslation {
    translations
        .iter()
        .find(|translation| translation.case().case_id() == case_id)
        .unwrap()
}

fn canonical(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn target_leaf(name: &str) -> &str {
    name.rsplit([':', '.', '/', '#'])
        .find(|part| !part.is_empty())
        .unwrap()
}

#[test]
fn fixture_translation_resolves_exact_targets_without_query_leakage() {
    let repository = FixtureRepository::new();
    let records = fixture_records();
    let symbols = source_symbols(&repository, &records);

    let translations = translate_fixture(&repository, &records, &symbols);

    assert_eq!(translations.len(), records.len());
    let eligible = translations
        .iter()
        .filter(|translation| matches!(translation.case().status(), CaseStatus::Eligible))
        .count();
    let unsupported = translations
        .iter()
        .filter(|translation| matches!(translation.case().status(), CaseStatus::Unsupported { .. }))
        .count();
    let invalid = translations
        .iter()
        .filter(|translation| matches!(translation.case().status(), CaseStatus::Invalid { .. }))
        .count();
    assert_eq!((eligible, unsupported, invalid), (3, 1, 0));

    for (record, translated) in records.iter().zip(&translations) {
        let case = translated.case();
        assert_eq!(case.query_policy().input(), record.description());
        assert_eq!(case.gold_evidence().sources().len(), 1);
        assert!(case.gold_evidence().derived_identifiers().is_empty());
        assert_eq!(
            case.raw_upstream()["upstream_extra"],
            json!(format!(
                "retained-{}provenance",
                match record.case_id() {
                    RUST_BUILD_CASE => "rust-",
                    "rust::synthetic/repo::format_value" => "second-rust-",
                    "typescript::synthetic/repo::render_widget" => "typescript-",
                    _ => "java-",
                }
            ))
        );

        let target_key = canonical(target_leaf(record.name()));
        assert!(canonical(record.path()).contains(&target_key));
        let serialized_policy = serde_json::to_string(case.query_policy()).unwrap();
        assert!(!canonical(&serialized_policy).contains(&target_key));
        assert!(!serialized_policy.contains(record.path()));
        assert!(!serialized_policy.contains(&format!(
            "{}..{}",
            record.start_line(),
            record.end_line()
        )));
    }

    let java = translation(&translations, "java::synthetic/repo::parse_item");
    assert_eq!(
        java.case().status().reason(),
        Some("SPUR has no Java language extractor in the pinned benchmark contract")
    );
    assert!(java.case().is_denominator_visible());
    assert!(java.model_score_input().is_none());
}

#[test]
fn canonical_target_name_leakage_makes_the_case_invalid_and_query_safe() {
    let repository = FixtureRepository::new();
    let records = records_with(|record| {
        record["description"] = json!("The BUILD-ANSWER routine increments its input.");
    });
    let symbols = source_symbols(&repository, &records);

    let translations = translate_fixture(&repository, &records, &symbols);
    let translated = translation(&translations, RUST_BUILD_CASE);

    assert!(matches!(
        translated.case().status(),
        CaseStatus::Invalid { .. }
    ));
    assert!(translated
        .case()
        .status()
        .reason()
        .unwrap()
        .contains("leaks the target name"));
    assert!(translated.case().is_denominator_visible());
    assert!(!canonical(translated.case().query_policy().input()).contains("buildanswer"));
    assert_eq!(translated.case().gold_evidence().sources().len(), 1);
    assert!(translated.model_score_input().is_none());
}

#[test]
fn incidental_target_substring_does_not_count_as_query_leakage() {
    let repository = FixtureRepository::new();
    let records = records_with(|record| {
        record["name"] = json!("crate::answer::cat");
        record["description"] = json!("Concatenates the supplied values.");
    });
    let symbols = source_symbols(&repository, &records);

    let translations = translate_fixture(&repository, &records, &symbols);
    let translated = translation(&translations, RUST_BUILD_CASE);

    assert!(matches!(translated.case().status(), CaseStatus::Eligible));
    assert_eq!(
        translated.case().query_policy().input(),
        "Concatenates the supplied values."
    );
    assert!(translated.model_score_input().is_some());
}

#[test]
fn unsupported_without_source_symbol_remains_visible_and_auditable() {
    let repository = FixtureRepository::new();
    let records = fixture_records();
    let mut symbols = source_symbols(&repository, &records);
    symbols.retain(|symbol| symbol.name() != "Repo.parseItem");

    let translations = translate_fixture(&repository, &records, &symbols);
    let translated = translation(&translations, "java::synthetic/repo::parse_item");

    assert!(matches!(
        translated.case().status(),
        CaseStatus::Unsupported { .. }
    ));
    assert_eq!(
        translated.case().status().reason(),
        Some("SPUR has no Java language extractor in the pinned benchmark contract")
    );
    assert!(translated.case().is_denominator_visible());
    assert!(translated.model_score_input().is_none());
    assert!(translated.case().gold_evidence().sources().is_empty());
    assert!(!translated
        .case()
        .gold_evidence()
        .derived_identifiers()
        .is_empty());
    assert_eq!(
        translated.case().raw_upstream()["upstream_extra"],
        json!("retained-java-provenance")
    );
}

#[test]
fn unresolved_target_is_invalid_without_empty_gold() {
    let repository = FixtureRepository::new();
    let records = fixture_records();
    let mut symbols = source_symbols(&repository, &records);
    symbols.retain(|symbol| symbol.name() != records[0].name());

    let translations = translate_fixture(&repository, &records, &symbols);
    let translated = translation(&translations, RUST_BUILD_CASE);

    assert_invalid_with_reason(translated, "did not resolve");
    assert!(translated.case().gold_evidence().sources().is_empty());
    assert_eq!(
        translated
            .case()
            .gold_evidence()
            .derived_identifiers()
            .len(),
        1
    );
}

#[test]
fn multiple_exact_targets_are_invalid() {
    let repository = FixtureRepository::new();
    let records = fixture_records();
    let mut symbols = source_symbols(&repository, &records);
    let duplicate = RepoQaSourceSymbol::new(
        records[0].name(),
        SourceIdentity::new(
            symbols[0].source().path(),
            symbols[0].source().byte_start(),
            symbols[0].source().byte_end(),
            Some("second-spur-symbol".to_owned()),
        )
        .unwrap(),
    )
    .unwrap();
    symbols.push(duplicate);

    let translations = translate_fixture(&repository, &records, &symbols);

    assert_invalid_with_reason(translation(&translations, RUST_BUILD_CASE), "multiple");
}

#[test]
fn target_path_or_span_mismatch_is_invalid() {
    let repository = FixtureRepository::new();
    let records = fixture_records();
    let mut symbols = source_symbols(&repository, &records);
    let original = &symbols[0];
    symbols[0] = RepoQaSourceSymbol::new(
        original.name(),
        SourceIdentity::new(
            "src/not-the-pinned-path.rs",
            original.source().byte_start(),
            original.source().byte_end(),
            Some("mismatched-spur-symbol".to_owned()),
        )
        .unwrap(),
    )
    .unwrap();

    let translations = translate_fixture(&repository, &records, &symbols);

    assert_invalid_with_reason(translation(&translations, RUST_BUILD_CASE), "path or span");
}

#[test]
fn malformed_and_out_of_range_spans_remain_invalid_and_visible() {
    let repository = FixtureRepository::new();
    let original = fixture_records();
    let symbols = source_symbols(&repository, &original);

    for records in [
        records_with(|record| {
            record["start_line"] = json!(2);
            record["end_line"] = json!(2);
        }),
        records_with(|record| {
            record["end_line"] = json!(100);
        }),
    ] {
        let translations = translate_fixture(&repository, &records, &symbols);
        let translated = translation(&translations, RUST_BUILD_CASE);
        assert_invalid_with_reason(translated, "line span");
        assert!(!translated
            .case()
            .gold_evidence()
            .derived_identifiers()
            .is_empty());
    }
}

#[test]
fn non_file_target_is_invalid() {
    let repository = FixtureRepository::new();
    let records = records_with(|record| {
        record["path"] = json!("src");
    });
    let symbols = source_symbols(&repository, &fixture_records());

    let translations = translate_fixture(&repository, &records, &symbols);

    assert_invalid_with_reason(translation(&translations, RUST_BUILD_CASE), "regular file");
}

#[cfg(unix)]
#[test]
fn symlink_escape_target_is_invalid() {
    use std::os::unix::fs::symlink;

    let repository = FixtureRepository::new();
    fs::write(repository.outside_path(), "fn escaped() {}\n").unwrap();
    symlink(
        repository.outside_path(),
        repository.root().join("src/escape.rs"),
    )
    .unwrap();
    let records = records_with(|record| {
        record["path"] = json!("src/escape.rs");
    });
    let symbols = source_symbols(&repository, &fixture_records());

    let translations = translate_fixture(&repository, &records, &symbols);

    assert_invalid_with_reason(translation(&translations, RUST_BUILD_CASE), "escapes");
}

#[test]
fn native_model_score_input_is_separate_and_contains_no_computed_score() {
    let repository = FixtureRepository::new();
    let records = fixture_records();
    let symbols = source_symbols(&repository, &records);
    let translations = translate_fixture(&repository, &records, &symbols);
    let translated = translation(&translations, RUST_BUILD_CASE);

    let model_input: &RepoQaModelScoreInput = translated.model_score_input().unwrap();
    assert_eq!(model_input.language().as_str(), "rust");
    assert_eq!(model_input.repository(), "synthetic/repo");
    assert_eq!(records[0].repository(), model_input.repository());
    assert_eq!(
        model_input.ground_truth_name(),
        "crate::answer::build_answer"
    );
    assert_eq!(model_input.targets().len(), 2);
    assert!(model_input
        .targets()
        .iter()
        .any(|target| target.name() == "crate::answer::format_value"));
    assert!(model_input
        .targets()
        .iter()
        .all(|target| target.source().symbol_id().is_some()));

    let model_json = serde_json::to_value(model_input).unwrap();
    assert!(model_json.get("score").is_none());
    assert!(model_json.get("model_output").is_none());
    assert!(model_json.get("query").is_none());

    let case_json = serde_json::to_value(translated.case()).unwrap();
    assert!(case_json["query_policy"].get("targets").is_none());
    assert_eq!(
        case_json["query_policy"]["input"],
        json!(records[0].description())
    );
}

fn assert_invalid_with_reason(translation: &RepoQaTranslation, expected: &str) {
    let CaseStatus::Invalid { reason } = translation.case().status() else {
        panic!(
            "expected invalid case, got {:?}",
            translation.case().status()
        );
    };
    assert!(
        reason.contains(expected),
        "unexpected invalid reason: {reason}"
    );
    assert!(
        translation.case().is_denominator_visible(),
        "invalid RepoQA cases must remain in denominator counts"
    );
    assert!(
        translation.model_score_input().is_none(),
        "invalid RepoQA cases must not enter the model lane"
    );
}
