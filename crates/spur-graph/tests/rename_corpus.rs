use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use spur_graph::extract::languages::Language;
use spur_graph::extract::tree_sitter::{BytesExtractor, ExtractedSymbol};
use spur_graph::git_walk::{try_rename_match, FileChange, FileChangeKind, SymbolChange};
use spur_graph::{
    stable_symbol_id_for, ChangeKind, GitPath, RenamePrev, SnapshotKey, SymbolSnapshotArtifact,
};

const MIN_CASES_PER_LANGUAGE: u32 = 50;

#[derive(Debug, Deserialize)]
struct Expected {
    #[serde(default)]
    expected_renames: Vec<RenamePair>,
    #[serde(default)]
    expected_added: Vec<String>,
    #[serde(default)]
    expected_deleted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
struct RenamePair {
    from: String,
    to: String,
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
}

fn run_corpus(language: Language, relative_dir: &str) -> CorpusStats {
    let lang_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_dir);
    let extension = extension_for(language);
    let mut stats = CorpusStats::default();
    let mut extractor = BytesExtractor::for_language(language).expect("create extractor");

    for entry in std::fs::read_dir(&lang_dir).expect("read corpus directory") {
        let path = entry.expect("read corpus entry").path();
        if !path.to_string_lossy().ends_with(".expected.json") {
            continue;
        }

        stats.cases += 1;
        let stem = path
            .file_stem()
            .expect("expected file stem")
            .to_string_lossy()
            .replace(".expected", "");
        let old_path = lang_dir.join(format!("{stem}.old.{extension}"));
        let new_path = lang_dir.join(format!("{stem}.new.{extension}"));
        let expected: Expected = serde_json::from_str(
            &std::fs::read_to_string(&path).expect("read expected corpus labels"),
        )
        .expect("parse expected corpus labels");
        let old_symbols = extractor
            .extract(
                &old_path,
                &std::fs::read(&old_path).expect("read old corpus blob"),
            )
            .expect("extract old corpus symbols");
        let new_symbols = extractor
            .extract(
                &new_path,
                &std::fs::read(&new_path).expect("read new corpus blob"),
            )
            .expect("extract new corpus symbols");

        let (deleted_candidates, added_candidates) =
            candidate_changes(&old_path, &new_path, &old_symbols, &new_symbols);
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
            path: logical_fixture_path(&stem, "new", extension).into(),
            kind: FileChangeKind::Modified,
            parent_sha: Some("old".to_string()),
        };

        let (changes, _diagnostics) =
            try_rename_match(deleted_candidates, added_candidates, &file_change, language);
        let predicted = predicted_renames(&changes, &deleted_name_by_key);
        let Expected {
            expected_renames,
            expected_added,
            expected_deleted,
        } = expected;
        let _non_rename_labels = expected_added.len() + expected_deleted.len();
        let expected: BTreeSet<_> = expected_renames.into_iter().collect();

        stats.tp += predicted.intersection(&expected).count() as u32;
        stats.fp += predicted.difference(&expected).count() as u32;
        stats.fn_ += expected.difference(&predicted).count() as u32;
    }

    stats
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

fn snapshot_from(commit: &str, path: &Path, symbol: &ExtractedSymbol) -> SymbolSnapshotArtifact {
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: stable_symbol_id_for(path, &symbol.entity_name, &symbol.anchor_hash),
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
