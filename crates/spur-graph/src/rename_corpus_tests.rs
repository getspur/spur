use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{try_rename_match, FileChange, FileChangeKind, SymbolChange};
use crate::extract::languages::Language;
use crate::extract::tree_sitter::{BytesExtractor, ExtractedSymbol};
use crate::identity::stable_symbol_id_for;
use crate::schema::{ChangeKind, GitPath, RenamePrev, SnapshotKey, SymbolSnapshotArtifact};

const MIN_CASES_PER_LANGUAGE: u32 = 50;

#[derive(Debug, Deserialize)]
struct Expected {
    #[serde(default)]
    class: Option<AdversarialClass>,
    #[serde(default)]
    expected_renames: Vec<RenamePair>,
    #[serde(default)]
    expected_added: Vec<String>,
    #[serde(default)]
    expected_deleted: Vec<String>,
    #[serde(default)]
    expected_ambiguous_candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
struct RenamePair {
    from: String,
    to: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AdversarialClass {
    FullRewrite,
    Crossover,
    ParamsOnly,
}

impl AdversarialClass {
    fn label(self) -> &'static str {
        match self {
            Self::FullRewrite => "full_rewrite",
            Self::Crossover => "crossover",
            Self::ParamsOnly => "params_only",
        }
    }
}

#[derive(Debug, Default)]
struct CorpusStats {
    cases: u32,
    tp: u32,
    fp: u32,
    fn_: u32,
}

impl CorpusStats {
    fn precision(&self) -> f64 {
        self.tp as f64 / (self.tp + self.fp).max(1) as f64
    }

    fn recall(&self) -> f64 {
        self.tp as f64 / (self.tp + self.fn_).max(1) as f64
    }

    fn f1(&self) -> f64 {
        let (precision, recall) = (self.precision(), self.recall());
        if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        }
    }

    fn record(&mut self, predicted: &BTreeSet<RenamePair>, expected: &BTreeSet<RenamePair>) {
        self.cases += 1;
        self.tp += predicted.intersection(expected).count() as u32;
        self.fp += predicted.difference(expected).count() as u32;
        self.fn_ += expected.difference(predicted).count() as u32;
    }
}

#[derive(Debug, Default)]
struct AdversarialStats {
    cases: u32,
    exact_outcomes: u32,
    renames: CorpusStats,
}

#[derive(Debug)]
struct FixtureOutcome {
    stem: String,
    expected: Expected,
    predicted_renames: BTreeSet<RenamePair>,
    predicted_added: BTreeSet<String>,
    predicted_deleted: BTreeSet<String>,
    diagnostics: Vec<String>,
}

fn run_corpus(language: Language, relative_dir: &str) -> CorpusStats {
    let lang_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_dir);
    let extension = extension_for(language);
    let mut stats = CorpusStats::default();
    let mut extractor = BytesExtractor::for_language(language).expect("create extractor");

    for path in flat_expected_files(&lang_dir) {
        let stem = flat_fixture_stem(&path);
        let old_path = lang_dir.join(format!("{stem}.old.{extension}"));
        let new_path = lang_dir.join(format!("{stem}.new.{extension}"));
        let outcome = run_fixture(&mut extractor, language, &stem, &old_path, &new_path, &path);
        let expected: BTreeSet<_> = outcome.expected.expected_renames.into_iter().collect();
        stats.record(&outcome.predicted_renames, &expected);
    }

    stats
}

fn run_adversarial_corpus(
    language: Language,
    relative_dir: &str,
) -> BTreeMap<AdversarialClass, AdversarialStats> {
    let lang_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_dir);
    let extension = extension_for(language);
    let mut extractor = BytesExtractor::for_language(language).expect("create extractor");
    let mut by_class = BTreeMap::<AdversarialClass, AdversarialStats>::new();

    for case_dir in adversarial_case_dirs(&lang_dir) {
        let stem = case_dir
            .file_name()
            .expect("adversarial fixture directory name")
            .to_string_lossy()
            .into_owned();
        let outcome = run_fixture(
            &mut extractor,
            language,
            &stem,
            &case_dir.join(format!("old.{extension}")),
            &case_dir.join(format!("new.{extension}")),
            &case_dir.join("expected.json"),
        );
        let class = outcome
            .expected
            .class
            .unwrap_or_else(|| panic!("{stem} expected.json must declare class"));
        let expected_renames: BTreeSet<_> =
            outcome.expected.expected_renames.iter().cloned().collect();
        assert_adversarial_outcome(language_name(language), &outcome);
        let class_stats = by_class.entry(class).or_default();
        class_stats.cases += 1;
        class_stats.exact_outcomes += 1;
        class_stats
            .renames
            .record(&outcome.predicted_renames, &expected_renames);
    }

    by_class
}

fn run_fixture(
    extractor: &mut BytesExtractor,
    language: Language,
    stem: &str,
    old_path: &Path,
    new_path: &Path,
    expected_path: &Path,
) -> FixtureOutcome {
    let extension = extension_for(language);
    let expected: Expected = serde_json::from_str(
        &std::fs::read_to_string(expected_path).expect("read expected corpus labels"),
    )
    .expect("parse expected corpus labels");
    let old_symbols = extractor
        .extract(
            old_path,
            &std::fs::read(old_path).expect("read old corpus blob"),
        )
        .expect("extract old corpus symbols");
    let new_symbols = extractor
        .extract(
            new_path,
            &std::fs::read(new_path).expect("read new corpus blob"),
        )
        .expect("extract new corpus symbols");

    let (deleted_candidates, added_candidates) =
        candidate_changes(old_path, new_path, &old_symbols, &new_symbols);
    let deleted_name_by_key: HashMap<_, _> = deleted_candidates
        .iter()
        .map(|change| {
            (
                change.snapshot.key.clone(),
                change.snapshot.entity_name.clone(),
            )
        })
        .collect();
    let file_change = FileChange {
        path: logical_fixture_path(stem, "new", extension).into(),
        kind: FileChangeKind::Modified,
        parent_sha: Some("old".to_string()),
    };

    let (changes, diagnostics) =
        try_rename_match(deleted_candidates, added_candidates, &file_change, language);
    let predicted_renames = predicted_renames(&changes, &deleted_name_by_key);
    let predicted_added = predicted_names(&changes, |kind| matches!(kind, ChangeKind::Added));
    let predicted_deleted = predicted_names(&changes, |kind| matches!(kind, ChangeKind::Deleted));

    FixtureOutcome {
        stem: stem.to_string(),
        expected,
        predicted_renames,
        predicted_added,
        predicted_deleted,
        diagnostics,
    }
}

fn candidate_changes(
    old_path: &Path,
    new_path: &Path,
    old_symbols: &[ExtractedSymbol],
    new_symbols: &[ExtractedSymbol],
) -> (Vec<SymbolChange>, Vec<SymbolChange>) {
    let mut left_by_identity: HashMap<_, _> = old_symbols
        .iter()
        .map(|symbol| (symbol_identity(symbol), symbol))
        .collect();
    let mut added = Vec::new();

    for symbol in new_symbols {
        if left_by_identity.remove(&symbol_identity(symbol)).is_some() {
            continue;
        }
        added.push(SymbolChange {
            snapshot: snapshot_from("new", new_path, symbol),
            change_kind: ChangeKind::Added,
            parent_sha: None,
        });
    }

    let deleted = left_by_identity
        .into_values()
        .map(|symbol| SymbolChange {
            snapshot: snapshot_from("old", old_path, symbol),
            change_kind: ChangeKind::Deleted,
            parent_sha: None,
        })
        .collect();

    (deleted, added)
}

fn predicted_renames(
    changes: &[SymbolChange],
    deleted_name_by_key: &HashMap<SnapshotKey, String>,
) -> BTreeSet<RenamePair> {
    changes
        .iter()
        .filter_map(|change| match &change.change_kind {
            ChangeKind::RenamedFrom(RenamePrev::Symbol(previous)) => {
                deleted_name_by_key.get(previous).map(|from| RenamePair {
                    from: from.clone(),
                    to: change.snapshot.entity_name.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn predicted_names(
    changes: &[SymbolChange],
    predicate: impl Fn(&ChangeKind) -> bool,
) -> BTreeSet<String> {
    changes
        .iter()
        .filter(|change| predicate(&change.change_kind))
        .map(|change| change.snapshot.entity_name.clone())
        .collect()
}

fn snapshot_from(commit: &str, path: &Path, symbol: &ExtractedSymbol) -> SymbolSnapshotArtifact {
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: stable_symbol_id_for(path, &symbol.entity_name),
            commit: commit.to_string(),
        },
        file_path: GitPath::from(path),
        entity_name: symbol.entity_name.clone(),
        symbol_kind: symbol.symbol_kind.clone(),
        enclosing_scope: symbol.enclosing_scope.clone(),
        byte_range: symbol.byte_range,
        line_range: symbol.line_range,
        anchor_hash: symbol.anchor_hash.clone(),
        tokens: symbol.tokens.clone(),
    }
}

fn flat_expected_files(lang_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(lang_dir)
        .expect("read corpus directory")
        .map(|entry| entry.expect("read corpus entry").path())
        .filter(|path| path.is_file() && path.to_string_lossy().ends_with(".expected.json"))
        .collect();
    paths.sort();
    paths
}

fn adversarial_case_dirs(lang_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(lang_dir)
        .expect("read corpus directory")
        .map(|entry| entry.expect("read corpus entry").path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("adversarial_"))
        })
        .collect();
    paths.sort();
    paths
}

fn flat_fixture_stem(path: &Path) -> String {
    path.file_stem()
        .expect("expected file stem")
        .to_string_lossy()
        .replace(".expected", "")
}

fn symbol_identity(symbol: &ExtractedSymbol) -> (String, String, Option<String>) {
    (
        symbol.entity_name.clone(),
        symbol.symbol_kind.clone(),
        symbol.enclosing_scope.clone(),
    )
}

fn logical_fixture_path(stem: &str, side: &str, extension: &str) -> PathBuf {
    PathBuf::from(format!("src/{stem}.{side}.{extension}"))
}

fn extension_for(language: Language) -> &'static str {
    match language {
        Language::Rust => "rs",
        Language::TypeScript => "ts",
        Language::Python => "py",
        Language::Tsx => "tsx",
        Language::Markdown => "md",
    }
}

fn language_name(language: Language) -> &'static str {
    match language {
        Language::Rust => "rust",
        Language::TypeScript => "typescript",
        Language::Python => "python",
        Language::Tsx => "tsx",
        Language::Markdown => "markdown",
    }
}

fn assert_baseline(language: &str, stats: CorpusStats, baseline: f64) {
    println!(
        "{language} cases={} F1={:.3} P={:.3} R={:.3} TP={} FP={} FN={}",
        stats.cases,
        stats.f1(),
        stats.precision(),
        stats.recall(),
        stats.tp,
        stats.fp,
        stats.fn_
    );
    assert!(
        stats.cases >= MIN_CASES_PER_LANGUAGE,
        "{language} corpus has {} cases; expected at least {MIN_CASES_PER_LANGUAGE}",
        stats.cases
    );
    assert!(
        stats.f1() >= baseline,
        "{language} F1 below {baseline:.2} baseline: {:.3}",
        stats.f1()
    );
}

fn assert_adversarial_outcome(language: &str, outcome: &FixtureOutcome) {
    let expected_renames: BTreeSet<_> = outcome.expected.expected_renames.iter().cloned().collect();
    let expected_added: BTreeSet<_> = outcome.expected.expected_added.iter().cloned().collect();
    let expected_deleted: BTreeSet<_> = outcome.expected.expected_deleted.iter().cloned().collect();

    assert_eq!(
        outcome.predicted_renames, expected_renames,
        "{language} {} predicted unexpected renames",
        outcome.stem
    );
    assert_eq!(
        outcome.predicted_added, expected_added,
        "{language} {} predicted unexpected Added set",
        outcome.stem
    );
    assert_eq!(
        outcome.predicted_deleted, expected_deleted,
        "{language} {} predicted unexpected Deleted set",
        outcome.stem
    );

    for candidate in &outcome.expected.expected_ambiguous_candidates {
        assert!(
            outcome.diagnostics.iter().any(|diagnostic| {
                diagnostic.contains("ambiguous_rename")
                    && diagnostic.contains(&format!("candidate={candidate}"))
            }),
            "{language} {} missing ambiguous_rename diagnostic for {candidate}; diagnostics={:#?}",
            outcome.stem,
            outcome.diagnostics
        );
    }
}

fn assert_adversarial_class(
    stats_by_class: &BTreeMap<AdversarialClass, AdversarialStats>,
    class: AdversarialClass,
    expected_cases: u32,
) {
    let stats = stats_by_class
        .get(&class)
        .unwrap_or_else(|| panic!("missing adversarial class {}", class.label()));
    println!(
        "adversarial {} cases={} exact={}/{} F1={:.3} P={:.3} R={:.3} TP={} FP={} FN={}",
        class.label(),
        stats.cases,
        stats.exact_outcomes,
        stats.cases,
        stats.renames.f1(),
        stats.renames.precision(),
        stats.renames.recall(),
        stats.renames.tp,
        stats.renames.fp,
        stats.renames.fn_
    );
    assert_eq!(
        stats.cases,
        expected_cases,
        "adversarial {} case count mismatch",
        class.label()
    );
    assert_eq!(
        stats.exact_outcomes,
        expected_cases,
        "adversarial {} exact outcomes mismatch",
        class.label()
    );
    assert_eq!(
        stats.renames.fp,
        0,
        "adversarial {} emitted false positive renames",
        class.label()
    );
    assert_eq!(
        stats.renames.fn_,
        0,
        "adversarial {} missed expected renames",
        class.label()
    );
}

#[test]
fn rust_corpus_f1_meets_baseline() {
    let stats = run_corpus(Language::Rust, "tests/fixtures/rename_corpus/rust");
    assert_baseline("rust", stats, 0.80);
}

#[test]
fn typescript_corpus_f1_meets_baseline() {
    let stats = run_corpus(
        Language::TypeScript,
        "tests/fixtures/rename_corpus/typescript",
    );
    assert_baseline("typescript", stats, 0.78);
}

#[test]
fn python_corpus_f1_meets_baseline() {
    let stats = run_corpus(Language::Python, "tests/fixtures/rename_corpus/python");
    assert_baseline("python", stats, 0.75);
}

#[test]
fn adversarial_corpus_has_explicit_outcomes_by_class() {
    let mut stats_by_class = BTreeMap::<AdversarialClass, AdversarialStats>::new();
    for (language, relative_dir) in [
        (Language::Rust, "tests/fixtures/rename_corpus/rust"),
        (
            Language::TypeScript,
            "tests/fixtures/rename_corpus/typescript",
        ),
        (Language::Python, "tests/fixtures/rename_corpus/python"),
    ] {
        for (class, stats) in run_adversarial_corpus(language, relative_dir) {
            let combined = stats_by_class.entry(class).or_default();
            combined.cases += stats.cases;
            combined.exact_outcomes += stats.exact_outcomes;
            combined.renames.cases += stats.renames.cases;
            combined.renames.tp += stats.renames.tp;
            combined.renames.fp += stats.renames.fp;
            combined.renames.fn_ += stats.renames.fn_;
        }
    }

    assert_adversarial_class(&stats_by_class, AdversarialClass::FullRewrite, 3);
    assert_adversarial_class(&stats_by_class, AdversarialClass::Crossover, 3);
    // Phase 1 token bags include parameter identifiers, so whole-parameter
    // renames can fall below the calibrated language thresholds.
    assert_adversarial_class(&stats_by_class, AdversarialClass::ParamsOnly, 3);
}
