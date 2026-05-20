use std::cmp::Ordering;

use crate::{GraphIndexArtifact, GraphSymbolArtifact, CODE_SYMBOL_URI_PREFIX};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectorResolution {
    Resolved(ResolvedSymbol),
    Ambiguous { candidates: Vec<CandidateRow> },
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSymbol {
    pub stable_symbol_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRow {
    pub selector: String,
    pub uri: String,
    pub id: String,
    pub qualified_name: String,
    pub file_path: String,
    pub line_range: [usize; 2],
    pub symbol_kind: String,
}

pub fn resolve_selector(artifact: &GraphIndexArtifact, selector: &str) -> SelectorResolution {
    let selector = selector.trim();
    if selector.is_empty() {
        return SelectorResolution::NotFound;
    }

    if let Some(symbol_id) = selector.strip_prefix(CODE_SYMBOL_URI_PREFIX) {
        return resolve_symbol_by_id(artifact, symbol_id)
            .map(SelectorResolution::Resolved)
            .unwrap_or(SelectorResolution::NotFound);
    }

    if is_bare_stable_symbol_id(selector) {
        if let Some(symbol) = resolve_symbol_by_id(artifact, selector) {
            return SelectorResolution::Resolved(symbol);
        }
    }

    if let Some(file_scoped) = selector
        .strip_prefix("file:")
        .or_else(|| selector.strip_prefix("path:"))
    {
        return resolve_file_scoped(artifact, file_scoped);
    }

    if let Some(line_resolution) = resolve_line_locator(artifact, selector) {
        return line_resolution;
    }

    if let Some(file_resolution) = resolve_file_qualified(artifact, selector) {
        return file_resolution;
    }

    if !first_token_contains_path_separator(selector) {
        let qualified_matches = artifact
            .symbols
            .iter()
            .filter(|symbol| symbol.qualified_name == selector)
            .collect();
        let resolution = resolution_from_matches(qualified_matches);
        if !matches!(resolution, SelectorResolution::NotFound) {
            return resolution;
        }
    }

    if selector.contains("::") {
        return SelectorResolution::NotFound;
    }

    let entity_matches = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.entity_name == selector)
        .collect();
    resolution_from_matches(entity_matches)
}

fn resolve_symbol_by_id(artifact: &GraphIndexArtifact, symbol_id: &str) -> Option<ResolvedSymbol> {
    if symbol_id.is_empty() {
        return None;
    }
    artifact
        .symbols
        .iter()
        .find(|symbol| symbol.stable_symbol_id == symbol_id)
        .map(resolved_symbol)
}

fn is_bare_stable_symbol_id(selector: &str) -> bool {
    // Production stable IDs are always 16 lowercase hex chars: truncated SHA-256
    // bytes interpreted as a u64 and formatted with `{:016x}`. Bare hex IDs
    // are a hint: an ID hit wins, but an ID miss falls through to file/line,
    // qualified-name, and entity_name selectors. The URI form remains the
    // explicit stable-ID-only escape hatch.
    selector.len() >= 16
        && selector
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn resolve_file_scoped(artifact: &GraphIndexArtifact, selector: &str) -> SelectorResolution {
    resolve_line_locator(artifact, selector)
        .or_else(|| resolve_file_qualified(artifact, selector))
        .unwrap_or(SelectorResolution::NotFound)
}

fn resolve_line_locator(
    artifact: &GraphIndexArtifact,
    selector: &str,
) -> Option<SelectorResolution> {
    let (file_path, line) = split_file_prefix(artifact, selector, ":")?;
    if line.starts_with(':') {
        return None;
    }
    if line.is_empty() || !line.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let Ok(line) = line.parse::<usize>() else {
        return None;
    };
    let symbol = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.file_path == file_path)
        .filter(|symbol| symbol.line_range[0] <= line && line <= symbol.line_range[1])
        .max_by(compare_innermost);
    Some(
        symbol
            .map(resolved_symbol)
            .map(SelectorResolution::Resolved)
            .unwrap_or(SelectorResolution::NotFound),
    )
}

fn resolve_file_qualified(
    artifact: &GraphIndexArtifact,
    selector: &str,
) -> Option<SelectorResolution> {
    let (file_path, chain) = split_file_prefix(artifact, selector, "::")
        .or_else(|| split_file_prefix(artifact, selector, ":"))?;
    let qualified_matches = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.file_path == file_path && symbol.qualified_name == chain)
        .collect::<Vec<_>>();
    let resolution = resolution_from_matches(qualified_matches);
    if !matches!(resolution, SelectorResolution::NotFound) {
        return Some(resolution);
    }

    let fallback_matches = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.file_path == file_path)
        .filter(|symbol| enclosing_scope_entity_name(symbol).as_deref() == Some(chain))
        .collect();
    Some(resolution_from_matches(fallback_matches))
}

fn split_file_prefix<'a>(
    artifact: &'a GraphIndexArtifact,
    selector: &'a str,
    separator: &str,
) -> Option<(&'a str, &'a str)> {
    artifact
        .files
        .iter()
        .filter_map(|file| {
            selector
                .strip_prefix(&file.file_path)
                .and_then(|tail| tail.strip_prefix(separator))
                .map(|tail| (file.file_path.as_str(), tail))
        })
        .max_by_key(|(file_path, _)| file_path.len())
}

fn first_token_contains_path_separator(selector: &str) -> bool {
    // A slash before the first `::` means the selector is path-shaped. File-qualified
    // matching has already had a chance to resolve it, so skip global qualified-name
    // matching to avoid treating path-like markdown section selectors as globals.
    selector
        .split("::")
        .next()
        .is_some_and(|token| token.contains('/'))
}

fn compare_innermost(left: &&GraphSymbolArtifact, right: &&GraphSymbolArtifact) -> Ordering {
    left.line_range[0]
        .cmp(&right.line_range[0])
        .then_with(|| right.line_range[1].cmp(&left.line_range[1]))
        .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
}

fn resolution_from_matches(symbols: Vec<&GraphSymbolArtifact>) -> SelectorResolution {
    match symbols.as_slice() {
        [] => SelectorResolution::NotFound,
        [symbol] => SelectorResolution::Resolved(resolved_symbol(symbol)),
        _ => SelectorResolution::Ambiguous {
            candidates: candidate_rows(symbols),
        },
    }
}

fn resolved_symbol(symbol: &GraphSymbolArtifact) -> ResolvedSymbol {
    ResolvedSymbol {
        stable_symbol_id: symbol.stable_symbol_id.clone(),
    }
}

fn candidate_rows(symbols: Vec<&GraphSymbolArtifact>) -> Vec<CandidateRow> {
    let mut candidates = symbols.into_iter().map(candidate_row).collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line_range[0].cmp(&right.line_range[0]))
            .then_with(|| left.line_range[1].cmp(&right.line_range[1]))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates
}

fn candidate_row(symbol: &GraphSymbolArtifact) -> CandidateRow {
    let uri = format!("{CODE_SYMBOL_URI_PREFIX}{}", symbol.stable_symbol_id);
    let selector = if symbol.qualified_name.is_empty() {
        uri.clone()
    } else {
        format!("{}::{}", symbol.file_path, symbol.qualified_name)
    };

    CandidateRow {
        selector,
        uri,
        id: symbol.stable_symbol_id.clone(),
        qualified_name: symbol.qualified_name.clone(),
        file_path: symbol.file_path.clone(),
        line_range: symbol.line_range,
        symbol_kind: symbol.symbol_kind.clone(),
    }
}

fn enclosing_scope_entity_name(symbol: &GraphSymbolArtifact) -> Option<String> {
    symbol
        .enclosing_scope
        .as_ref()
        .map(|scope| format!("{scope}::{}", symbol.entity_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphFileArtifact, GraphIndexArtifact, GraphIndexHeader, GraphSymbolArtifact};

    fn assert_resolved(artifact: &GraphIndexArtifact, selector: &str, expected_id: &str) {
        assert_eq!(
            resolve_selector(artifact, selector),
            SelectorResolution::Resolved(ResolvedSymbol {
                stable_symbol_id: expected_id.to_string(),
            })
        );
    }

    fn assert_not_found(artifact: &GraphIndexArtifact, selector: &str) {
        assert_eq!(
            resolve_selector(artifact, selector),
            SelectorResolution::NotFound
        );
    }

    fn file(path: &str) -> GraphFileArtifact {
        GraphFileArtifact {
            stable_file_id: format!("file-{path}"),
            file_path: path.to_string(),
        }
    }

    fn symbol(
        id: &str,
        file_path: &str,
        line_range: [usize; 2],
        entity_name: &str,
        qualified_name: &str,
        symbol_kind: &str,
        enclosing_scope: Option<&str>,
    ) -> GraphSymbolArtifact {
        GraphSymbolArtifact {
            stable_symbol_id: id.to_string(),
            file_path: file_path.to_string(),
            byte_range: [0, 8],
            line_range,
            entity_name: entity_name.to_string(),
            qualified_name: qualified_name.to_string(),
            symbol_kind: symbol_kind.to_string(),
            anchor_hash: format!("anchor-{id}"),
            enclosing_scope: enclosing_scope.map(str::to_string),
        }
    }

    fn artifact() -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: vec![
                file("src/cache.rs"),
                file("src/build.rs"),
                file("a/file.rs"),
                file("b/file.rs"),
                file("c/file.rs"),
                file("Scope"),
                file("src/runner.rs"),
                file("docs/guide.md"),
            ],
            symbols: vec![
                symbol(
                    "aaaaaaaaaaaaaaaa",
                    "src/cache.rs",
                    [10, 80],
                    "Cache",
                    "Cache",
                    "struct",
                    None,
                ),
                symbol(
                    "bbbbbbbbbbbbbbbb",
                    "src/cache.rs",
                    [20, 70],
                    "Cache",
                    "impl Cache",
                    "impl",
                    None,
                ),
                symbol(
                    "cccccccccccccccc",
                    "src/cache.rs",
                    [30, 40],
                    "run",
                    "impl Cache::run",
                    "method",
                    Some("impl Cache"),
                ),
                symbol(
                    "dddddddddddddddd",
                    "src/build.rs",
                    [1, 5],
                    "init",
                    "init",
                    "function",
                    None,
                ),
                symbol(
                    "eeeeeeeeeeeeeeee",
                    "src/build.rs",
                    [10, 20],
                    "make",
                    "Builder::make",
                    "function",
                    Some("Builder"),
                ),
                symbol(
                    "abababababababab",
                    "src/build.rs",
                    [30, 34],
                    "render",
                    "legacy render",
                    "method",
                    Some("View"),
                ),
                symbol(
                    "1000000000000000",
                    "a/file.rs",
                    [5, 6],
                    "Thing",
                    "duplicate::Thing",
                    "struct",
                    Some("duplicate"),
                ),
                symbol(
                    "2000000000000000",
                    "b/file.rs",
                    [3, 4],
                    "Thing",
                    "duplicate::Thing",
                    "struct",
                    Some("duplicate"),
                ),
                symbol(
                    "3000000000000000",
                    "c/file.rs",
                    [7, 8],
                    "Thing",
                    "duplicate::Thing",
                    "struct",
                    Some("duplicate"),
                ),
                symbol(
                    "4000000000000000",
                    "a/file.rs",
                    [25, 26],
                    "flush",
                    "Alpha::flush",
                    "method",
                    Some("Alpha"),
                ),
                symbol(
                    "5000000000000000",
                    "b/file.rs",
                    [30, 31],
                    "flush",
                    "Beta::flush",
                    "method",
                    Some("Beta"),
                ),
                symbol(
                    "6000000000000000",
                    "c/file.rs",
                    [35, 36],
                    "flush",
                    "Gamma::flush",
                    "method",
                    Some("Gamma"),
                ),
                symbol(
                    "7000000000000000",
                    "Scope",
                    [1, 3],
                    "run",
                    "run",
                    "function",
                    None,
                ),
                symbol(
                    "8000000000000000",
                    "src/runner.rs",
                    [1, 3],
                    "run",
                    "Scope::run",
                    "method",
                    Some("Scope"),
                ),
                symbol(
                    "9000000000000000",
                    "docs/guide.md",
                    [11, 14],
                    "Deep Dive",
                    "Overview::Deep Dive",
                    "section",
                    Some("Overview"),
                ),
            ],
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn resolves_uri_and_bare_hex_id_directly() {
        let artifact = artifact();

        assert_resolved(
            &artifact,
            "graph://symbol/aaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaa",
        );
        assert_resolved(&artifact, "dddddddddddddddd", "dddddddddddddddd");
    }

    #[test]
    fn production_length_bare_hex_selector_resolves_by_id() {
        let mut artifact = artifact();
        let expected_id = "abcdef0123456789";
        artifact.symbols.push(symbol(
            expected_id,
            "src/build.rs",
            [40, 41],
            "bare_id_only_target",
            "bare_id_only_target",
            "function",
            None,
        ));

        assert_resolved(&artifact, expected_id, expected_id);
    }

    #[test]
    fn sixteen_hex_selector_can_resolve_as_qualified_name() {
        let mut artifact = artifact();
        let selector = "abcdefabcdefabcd";
        let expected_id = "hex-qualified-symbol-id";
        artifact.symbols.push(symbol(
            expected_id,
            "src/build.rs",
            [40, 41],
            "hex_qualified",
            selector,
            "function",
            None,
        ));

        assert_resolved(&artifact, selector, expected_id);
    }

    #[test]
    fn sixteen_hex_selector_can_resolve_as_entity_name() {
        let mut artifact = artifact();
        let selector = "abcdefabcdefabcd";
        let expected_id = "hex-entity-symbol-id";
        artifact.symbols.push(symbol(
            expected_id,
            "src/build.rs",
            [40, 41],
            selector,
            "module::hex_entity",
            "function",
            None,
        ));

        assert_resolved(&artifact, selector, expected_id);
    }

    #[test]
    fn uri_symbol_selector_does_not_fall_through_on_missing_id() {
        let mut artifact = artifact();
        let selector = "abcdef0123456789";
        artifact.symbols.push(symbol(
            "hex-qualified-symbol-id",
            "src/build.rs",
            [40, 41],
            "hex_qualified",
            selector,
            "function",
            None,
        ));

        assert_not_found(&artifact, "graph://symbol/abcdef0123456789");
    }

    #[test]
    fn path_line_resolves_enclosing_symbol_and_prefers_innermost_overlap() {
        let artifact = artifact();

        assert_resolved(&artifact, "src/cache.rs:25", "bbbbbbbbbbbbbbbb");
        assert_resolved(&artifact, "src/cache.rs:35", "cccccccccccccccc");
    }

    #[test]
    fn path_line_returns_not_found_when_no_symbol_encloses_line() {
        let artifact = artifact();

        assert_not_found(&artifact, "src/cache.rs:90");
    }

    #[test]
    fn file_scoped_nonnumeric_tail_falls_through_to_qualified_name() {
        let mut artifact = artifact();
        let expected_id = "notaline-symbol-id";
        artifact.symbols.push(symbol(
            expected_id,
            "src/cache.rs",
            [90, 100],
            "NotALine",
            "NotALine",
            "function",
            None,
        ));

        assert_resolved(&artifact, "file:src/cache.rs:NotALine", expected_id);
    }

    #[test]
    fn unprefixed_path_colon_qualified_name_falls_through_to_file_qualified() {
        let artifact = artifact();
        let double_colon_resolution = resolve_selector(&artifact, "src/cache.rs::Cache");

        assert_eq!(
            double_colon_resolution,
            SelectorResolution::Resolved(ResolvedSymbol {
                stable_symbol_id: "aaaaaaaaaaaaaaaa".to_string(),
            })
        );
        assert_eq!(
            resolve_selector(&artifact, "src/cache.rs:Cache"),
            double_colon_resolution
        );
        assert_resolved(&artifact, "src/cache.rs:25", "bbbbbbbbbbbbbbbb");
    }

    #[test]
    fn file_qualified_name_resolves_direct_hit() {
        let artifact = artifact();

        assert_resolved(&artifact, "src/cache.rs::Cache", "aaaaaaaaaaaaaaaa");
    }

    #[test]
    fn file_chain_falls_back_to_enclosing_scope_and_entity_name() {
        let artifact = artifact();

        assert_resolved(&artifact, "src/build.rs::View::render", "abababababababab");
    }

    #[test]
    fn qualified_name_resolves_single_global_match() {
        let artifact = artifact();

        assert_resolved(&artifact, "Builder::make", "eeeeeeeeeeeeeeee");
    }

    #[test]
    fn entity_name_resolves_single_global_match() {
        let mut artifact = artifact();
        let expected_id = "qualified-name-symbol-id";
        artifact.symbols.push(symbol(
            expected_id,
            "src/build.rs",
            [40, 41],
            "my_func",
            "my_mod::my_func",
            "function",
            None,
        ));

        assert_resolved(&artifact, "my_func", expected_id);
    }

    #[test]
    fn qualified_name_with_multiple_matches_returns_ambiguous_candidates() {
        let artifact = artifact();

        let resolution = resolve_selector(&artifact, "duplicate::Thing");

        assert_eq!(
            resolution,
            SelectorResolution::Ambiguous {
                candidates: vec![
                    CandidateRow {
                        selector: "a/file.rs::duplicate::Thing".to_string(),
                        uri: "graph://symbol/1000000000000000".to_string(),
                        id: "1000000000000000".to_string(),
                        qualified_name: "duplicate::Thing".to_string(),
                        file_path: "a/file.rs".to_string(),
                        line_range: [5, 6],
                        symbol_kind: "struct".to_string(),
                    },
                    CandidateRow {
                        selector: "b/file.rs::duplicate::Thing".to_string(),
                        uri: "graph://symbol/2000000000000000".to_string(),
                        id: "2000000000000000".to_string(),
                        qualified_name: "duplicate::Thing".to_string(),
                        file_path: "b/file.rs".to_string(),
                        line_range: [3, 4],
                        symbol_kind: "struct".to_string(),
                    },
                    CandidateRow {
                        selector: "c/file.rs::duplicate::Thing".to_string(),
                        uri: "graph://symbol/3000000000000000".to_string(),
                        id: "3000000000000000".to_string(),
                        qualified_name: "duplicate::Thing".to_string(),
                        file_path: "c/file.rs".to_string(),
                        line_range: [7, 8],
                        symbol_kind: "struct".to_string(),
                    },
                ],
            }
        );
    }

    #[test]
    fn candidate_selector_uses_uri_when_qualified_name_is_empty() {
        let legacy_symbol = symbol(
            "legacy-empty-qualified-id",
            "src/cache.rs",
            [90, 91],
            "Legacy",
            "",
            "function",
            None,
        );

        let row = candidate_row(&legacy_symbol);

        assert_eq!(row.selector, "graph://symbol/legacy-empty-qualified-id");
        assert_eq!(row.selector, row.uri);
    }

    #[test]
    fn bare_name_with_multiple_matches_returns_ambiguous_candidates() {
        let artifact = artifact();

        let resolution = resolve_selector(&artifact, "flush");

        let SelectorResolution::Ambiguous { candidates } = resolution else {
            panic!("expected ambiguous resolution");
        };
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.selector.as_str())
                .collect::<Vec<_>>(),
            vec![
                "a/file.rs::Alpha::flush",
                "b/file.rs::Beta::flush",
                "c/file.rs::Gamma::flush",
            ]
        );
    }

    #[test]
    fn file_escape_forces_file_interpretation_for_scope_like_path() {
        let artifact = artifact();

        assert_resolved(&artifact, "file:Scope::run", "7000000000000000");
    }

    #[test]
    fn path_token_with_slash_and_existing_file_resolves() {
        let artifact = artifact();

        assert!(first_token_contains_path_separator(
            "docs/guide.md::Overview::Deep Dive"
        ));
        assert_resolved(
            &artifact,
            "docs/guide.md::Overview::Deep Dive",
            "9000000000000000",
        );
    }

    #[test]
    fn empty_or_whitespace_selector_returns_not_found() {
        let artifact = artifact();

        assert_not_found(&artifact, "");
        assert_not_found(&artifact, "   \n\t  ");
    }

    #[test]
    fn selector_with_spaces_and_markdown_special_chars_resolves() {
        let artifact = artifact();

        assert_resolved(&artifact, "impl Cache::run", "cccccccccccccccc");
    }
}
