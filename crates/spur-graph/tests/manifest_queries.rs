use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const BUILD_RS: &str = include_str!("../src/store/build.rs");
const LANGUAGE_QUERIES_RS: &str = include_str!("../src/extract/languages.rs");
const TREE_SITTER_RS: &str = include_str!("../src/extract/tree_sitter.rs");

#[test]
fn manifest_registers_every_extraction_query() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let query_dir = manifest_dir.join("queries");

    let query_files = query_files_from_dir(&query_dir);
    let consumed_queries =
        query_paths_from_sources(&[LANGUAGE_QUERIES_RS, TREE_SITTER_RS], "include_str");
    let registered_queries = query_paths_from_sources(&[BUILD_RS], "include_bytes");

    let unconsumed_files = query_files
        .difference(&consumed_queries)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        unconsumed_files.is_empty(),
        "query files not consumed by extraction: {unconsumed_files:#?}"
    );

    let missing = consumed_queries
        .difference(&registered_queries)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "extraction query files missing from MANIFEST_QUERY_BYTES: {missing:#?}"
    );
}

fn query_files_from_dir(query_dir: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    collect_query_files(query_dir, query_dir, &mut paths);
    paths
}

fn collect_query_files(root: &Path, dir: &Path, paths: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).expect("failed to read query directory") {
        let entry = entry.expect("failed to read query directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_query_files(root, &path, paths);
        } else if path.extension().is_some_and(|extension| extension == "scm") {
            paths.insert(query_path(root, &path));
        }
    }
}

fn query_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("query path must be below query root")
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn query_paths_from_sources(sources: &[&str], macro_name: &str) -> BTreeSet<String> {
    sources
        .iter()
        .flat_map(|source| query_paths_from_source(source, macro_name))
        .collect()
}

fn query_paths_from_source(source: &str, macro_name: &str) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let mut remaining = source;
    let needle = format!("{macro_name}!");

    while let Some(index) = remaining.find(&needle) {
        remaining = &remaining[index + needle.len()..];
        let Some(start) = remaining.find('"') else {
            break;
        };
        remaining = &remaining[start + 1..];
        let Some(end) = remaining.find('"') else {
            break;
        };
        if let Some(path) = normalize_query_include(&remaining[..end]) {
            paths.insert(path.to_string());
        }
        remaining = &remaining[end + 1..];
    }

    paths
}

fn normalize_query_include(path: &str) -> Option<&str> {
    path.strip_prefix("../../queries/")
        .or_else(|| path.strip_prefix("../queries/"))
        .filter(|path| path.ends_with(".scm"))
}
