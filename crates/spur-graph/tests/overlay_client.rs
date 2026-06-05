use std::collections::HashSet;
use std::sync::Arc;

use spur_graph::{
    CodeSelectorResolution, Confidence, GraphEdgeArtifact, GraphEdgeKind, GraphFileArtifact,
    GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader, GraphQueryClient as _,
    GraphSymbolArtifact, InMemoryClient, NodeId, OverlayClient, OwnedCalleeRecord,
    OwnedCallerRecord, RelationKind, SearchFilters, SearchMode, SearchOptions,
};

#[test]
fn empty_delta_matches_base_across_query_surface() {
    let base_artifact = base_artifact();
    let base = InMemoryClient::new(Arc::new(base_artifact));
    let overlay = OverlayClient::from_artifacts(
        base.clone(),
        Arc::new(empty_delta_artifact()),
        HashSet::new(),
    )
    .expect("overlay");

    assert_clients_match(&overlay, &base, "base-target-id");
    assert!(Arc::ptr_eq(
        &overlay.temporal_index(),
        &base.temporal_index()
    ));
}

#[test]
fn stable_changed_file_edit_matches_full_rebuild() {
    let overlay = overlay_from_parts(
        base_artifact(),
        artifact(
            "delta-edit",
            vec![
                file("src/changed.rs"),
                file("src/unchanged.rs"),
                file("src/deleted.rs"),
            ],
            vec![symbol(
                "base-target-id",
                "src/changed.rs",
                [3, 8],
                "target",
                "target",
            )],
            vec![],
        ),
        ["src/changed.rs"],
    );
    let full = InMemoryClient::new(Arc::new(artifact(
        "full-edit",
        vec![
            file("src/changed.rs"),
            file("src/unchanged.rs"),
            file("src/deleted.rs"),
        ],
        vec![
            symbol(
                "base-target-id",
                "src/changed.rs",
                [3, 8],
                "target",
                "target",
            ),
            caller_symbol(),
            deleted_symbol(),
        ],
        vec![
            edge(
                "unchanged-caller-id",
                Some("base-target-id"),
                Some("target"),
            ),
            edge("unchanged-caller-id", None, Some("fresh")),
            edge("unchanged-caller-id", Some("deleted-id"), Some("gone")),
        ],
    )));

    assert_clients_match(&overlay, &full, "base-target-id");
}

#[test]
fn added_symbol_called_by_unchanged_file_matches_full_rebuild() {
    let overlay = overlay_from_parts(
        base_artifact(),
        artifact(
            "delta-add",
            vec![
                file("src/changed.rs"),
                file("src/unchanged.rs"),
                file("src/deleted.rs"),
            ],
            vec![
                symbol(
                    "base-target-id",
                    "src/changed.rs",
                    [1, 2],
                    "target",
                    "target",
                ),
                symbol("fresh-id", "src/changed.rs", [10, 12], "fresh", "fresh"),
            ],
            vec![],
        ),
        ["src/changed.rs"],
    );
    let full = InMemoryClient::new(Arc::new(artifact(
        "full-add",
        vec![
            file("src/changed.rs"),
            file("src/unchanged.rs"),
            file("src/deleted.rs"),
        ],
        vec![
            symbol(
                "base-target-id",
                "src/changed.rs",
                [1, 2],
                "target",
                "target",
            ),
            symbol("fresh-id", "src/changed.rs", [10, 12], "fresh", "fresh"),
            caller_symbol(),
            deleted_symbol(),
        ],
        vec![
            edge(
                "unchanged-caller-id",
                Some("base-target-id"),
                Some("target"),
            ),
            edge("unchanged-caller-id", Some("deleted-id"), Some("gone")),
            edge("unchanged-caller-id", Some("fresh-id"), Some("fresh")),
        ],
    )));

    assert_clients_match(&overlay, &full, "fresh-id");
    assert_eq!(
        caller_records(overlay.find_caller_edges("fresh-id")),
        caller_records(full.find_caller_edges("fresh-id"))
    );
}

#[test]
fn renamed_symbol_called_by_unchanged_file_is_repointed_to_new_id() {
    let overlay = overlay_from_parts(
        base_artifact(),
        artifact(
            "delta-rename",
            vec![
                file("src/changed.rs"),
                file("src/unchanged.rs"),
                file("src/deleted.rs"),
            ],
            vec![symbol(
                "renamed-target-id",
                "src/changed.rs",
                [1, 2],
                "renamed",
                "renamed",
            )],
            vec![],
        ),
        ["src/changed.rs"],
    );
    let full = InMemoryClient::new(Arc::new(artifact(
        "full-rename",
        vec![
            file("src/changed.rs"),
            file("src/unchanged.rs"),
            file("src/deleted.rs"),
        ],
        vec![
            symbol(
                "renamed-target-id",
                "src/changed.rs",
                [1, 2],
                "renamed",
                "renamed",
            ),
            caller_symbol(),
            deleted_symbol(),
        ],
        vec![
            edge(
                "unchanged-caller-id",
                Some("renamed-target-id"),
                Some("renamed"),
            ),
            edge("unchanged-caller-id", None, Some("fresh")),
            edge("unchanged-caller-id", Some("deleted-id"), Some("gone")),
        ],
    )));

    assert_clients_match(&overlay, &full, "renamed-target-id");
    assert_eq!(
        caller_records(overlay.find_caller_edges("renamed-target-id")),
        caller_records(full.find_caller_edges("renamed-target-id"))
    );
}

#[test]
fn deleted_symbol_is_shadowed_and_unchanged_callee_becomes_unresolved() {
    let overlay = overlay_from_parts(
        base_artifact(),
        artifact(
            "delta-delete",
            vec![
                file("src/changed.rs"),
                file("src/unchanged.rs"),
                file("src/deleted.rs"),
            ],
            vec![symbol(
                "base-target-id",
                "src/changed.rs",
                [1, 2],
                "target",
                "target",
            )],
            vec![],
        ),
        ["src/changed.rs", "src/deleted.rs"],
    );
    let full = InMemoryClient::new(Arc::new(artifact(
        "full-delete",
        vec![
            file("src/changed.rs"),
            file("src/unchanged.rs"),
            file("src/deleted.rs"),
        ],
        vec![
            symbol(
                "base-target-id",
                "src/changed.rs",
                [1, 2],
                "target",
                "target",
            ),
            caller_symbol(),
        ],
        vec![
            edge(
                "unchanged-caller-id",
                Some("base-target-id"),
                Some("target"),
            ),
            edge("unchanged-caller-id", None, Some("fresh")),
            edge("unchanged-caller-id", None, Some("gone")),
        ],
    )));

    assert_eq!(
        overlay
            .symbol_by_id("deleted-id")
            .expect("overlay symbol query"),
        None
    );
    assert_clients_match(&overlay, &full, "deleted-id");
}

#[test]
fn changed_file_adding_call_to_unchanged_symbol_is_reflected_in_callers() {
    // base: `mover` (in the to-be-changed file) does NOT yet call `unchanged_target`.
    let base = artifact(
        "base-newcall",
        vec![file("src/changed.rs"), file("src/unchanged.rs")],
        vec![
            symbol("mover-id", "src/changed.rs", [1, 4], "mover", "mover"),
            symbol(
                "unchanged-target-id",
                "src/unchanged.rs",
                [10, 14],
                "unchanged_target",
                "unchanged_target",
            ),
        ],
        vec![],
    );
    // delta (changed.rs only): `mover` now calls `unchanged_target`. The target lives
    // outside the delta's extraction scope, so the edge is UNRESOLVED in the delta —
    // exactly what build_facts_for_paths produces for a cross-boundary call.
    let delta = artifact(
        "delta-newcall",
        vec![file("src/changed.rs")],
        vec![symbol(
            "mover-id",
            "src/changed.rs",
            [1, 6],
            "mover",
            "mover",
        )],
        vec![edge("mover-id", None, Some("unchanged_target"))],
    );
    let overlay = overlay_from_parts(base, delta, ["src/changed.rs"]);

    // Oracle: a full rebuild resolves the edge to the unchanged target.
    let full = InMemoryClient::new(Arc::new(artifact(
        "full-newcall",
        vec![file("src/changed.rs"), file("src/unchanged.rs")],
        vec![
            symbol("mover-id", "src/changed.rs", [1, 6], "mover", "mover"),
            symbol(
                "unchanged-target-id",
                "src/unchanged.rs",
                [10, 14],
                "unchanged_target",
                "unchanged_target",
            ),
        ],
        vec![edge(
            "mover-id",
            Some("unchanged-target-id"),
            Some("unchanged_target"),
        )],
    )));

    assert_eq!(
        caller_records(overlay.find_caller_edges("unchanged-target-id")),
        caller_records(full.find_caller_edges("unchanged-target-id")),
        "find_caller_edges(unchanged-target-id) must include the new caller from the changed file"
    );
}

fn assert_clients_match(
    actual: &impl spur_graph::GraphQueryClient,
    expected: &impl spur_graph::GraphQueryClient,
    focus_id: &str,
) {
    for options in [
        options("target", SearchMode::Exact),
        options("fresh", SearchMode::Exact),
        options("renamed", SearchMode::Exact),
        options("ta", SearchMode::Substring),
        SearchOptions {
            query: "t".to_owned(),
            mode: SearchMode::Substring,
            filters: SearchFilters {
                file: Some("src/changed.rs".to_owned()),
                ..SearchFilters::default()
            },
            limit: 1,
        },
    ] {
        assert_eq!(
            actual.search_symbols(&options).expect("actual search"),
            expected.search_symbols(&options).expect("expected search"),
            "search options: {options:?}"
        );
    }

    for sid in [
        "base-target-id",
        "fresh-id",
        "renamed-target-id",
        "deleted-id",
        "unchanged-caller-id",
    ] {
        assert_eq!(
            actual.symbol_by_id(sid).expect("actual symbol by id"),
            expected.symbol_by_id(sid).expect("expected symbol by id"),
            "symbol_by_id({sid})"
        );
    }

    for path in ["src/changed.rs", "src/unchanged.rs", "src/deleted.rs"] {
        assert_eq!(
            actual
                .symbols_by_file(path)
                .expect("actual symbols by file"),
            expected
                .symbols_by_file(path)
                .expect("expected symbols by file"),
            "symbols_by_file({path})"
        );
        assert_eq!(
            actual
                .file_manifest_by_path(path)
                .expect("actual manifest by path"),
            expected
                .file_manifest_by_path(path)
                .expect("expected manifest by path"),
            "file_manifest_by_path({path})"
        );
        assert_eq!(
            actual.file_exists(path).expect("actual file exists"),
            expected.file_exists(path).expect("expected file exists"),
            "file_exists({path})"
        );
    }

    for (path, name) in [
        ("src/changed.rs", "target"),
        ("src/changed.rs", "fresh"),
        ("src/changed.rs", "renamed"),
        ("src/deleted.rs", "gone"),
        ("src/unchanged.rs", "unchanged_caller"),
    ] {
        assert_eq!(
            actual
                .symbols_by_path_name(path, name)
                .expect("actual symbols by path name"),
            expected
                .symbols_by_path_name(path, name)
                .expect("expected symbols by path name"),
            "symbols_by_path_name({path}, {name})"
        );
    }

    for selector in [
        "target",
        "fresh",
        "renamed",
        "gone",
        "src/changed.rs::target",
        "src/changed.rs::renamed",
        "src/deleted.rs::gone",
    ] {
        assert_eq!(
            normalize_resolution(actual.resolve_selector(selector).expect("actual selector")),
            normalize_resolution(
                expected
                    .resolve_selector(selector)
                    .expect("expected selector")
            ),
            "resolve_selector({selector})"
        );
    }

    assert_eq!(
        callee_records(actual.find_callee_edges("unchanged-caller-id")),
        callee_records(expected.find_callee_edges("unchanged-caller-id")),
        "find_callee_edges(unchanged-caller-id)"
    );
    assert_eq!(
        caller_records(actual.find_caller_edges(focus_id)),
        caller_records(expected.find_caller_edges(focus_id)),
        "find_caller_edges({focus_id})"
    );
}

fn normalize_resolution(resolution: CodeSelectorResolution) -> CodeSelectorResolution {
    resolution
}

fn overlay_from_parts(
    base: GraphIndexArtifact,
    delta: GraphIndexArtifact,
    shadowed: impl IntoIterator<Item = &'static str>,
) -> OverlayClient<InMemoryClient> {
    OverlayClient::from_artifacts(
        InMemoryClient::new(Arc::new(base)),
        Arc::new(delta),
        shadowed.into_iter().map(str::to_owned).collect(),
    )
    .expect("overlay")
}

fn base_artifact() -> GraphIndexArtifact {
    artifact(
        "overlay-base",
        vec![
            file("src/changed.rs"),
            file("src/unchanged.rs"),
            file("src/deleted.rs"),
        ],
        vec![
            symbol(
                "base-target-id",
                "src/changed.rs",
                [1, 2],
                "target",
                "target",
            ),
            caller_symbol(),
            deleted_symbol(),
        ],
        vec![
            edge(
                "unchanged-caller-id",
                Some("base-target-id"),
                Some("target"),
            ),
            edge("unchanged-caller-id", None, Some("fresh")),
            edge("unchanged-caller-id", Some("deleted-id"), Some("gone")),
        ],
    )
}

fn empty_delta_artifact() -> GraphIndexArtifact {
    artifact("empty-delta", Vec::new(), Vec::new(), Vec::new())
}

fn caller_symbol() -> GraphSymbolArtifact {
    symbol(
        "unchanged-caller-id",
        "src/unchanged.rs",
        [20, 24],
        "unchanged_caller",
        "unchanged_caller",
    )
}

fn deleted_symbol() -> GraphSymbolArtifact {
    symbol("deleted-id", "src/deleted.rs", [30, 31], "gone", "gone")
}

fn artifact(
    graph_content_hash: &str,
    files: Vec<GraphFileArtifact>,
    symbols: Vec<GraphSymbolArtifact>,
    edges: Vec<GraphEdgeArtifact>,
) -> GraphIndexArtifact {
    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "test".to_owned(),
            content_hash_blake3: None,
        },
        manifest_version: "test".to_owned(),
        graph_content_hash: graph_content_hash.to_owned(),
        file_manifests: files
            .iter()
            .enumerate()
            .map(|(index, file)| GraphFileManifestEntry {
                stable_file_id: file.stable_file_id.clone(),
                path: file.file_path.clone(),
                content_oid: format!("{:040x}", index + 1),
                node_ids: vec![NodeId((index + 1) as u64)],
            })
            .collect(),
        file_node_ids: (1..=files.len()).map(|id| NodeId(id as u64)).collect(),
        files,
        symbol_node_ids: (101..101 + symbols.len())
            .map(|id| NodeId(id as u64))
            .collect(),
        symbols,
        edges,
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}

fn file(path: &str) -> GraphFileArtifact {
    GraphFileArtifact {
        stable_file_id: format!("file:{path}"),
        file_path: path.to_owned(),
    }
}

fn symbol(
    stable_symbol_id: &str,
    file_path: &str,
    line_range: [usize; 2],
    entity_name: &str,
    qualified_name: &str,
) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: stable_symbol_id.to_owned(),
        file_path: file_path.to_owned(),
        byte_range: [line_range[0], line_range[1]],
        line_range,
        entity_name: entity_name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        symbol_kind: "function".to_owned(),
        anchor_hash: format!("hash:{stable_symbol_id}"),
        enclosing_scope: None,
    }
}

fn edge(
    source_stable_symbol_id: &str,
    target_stable_symbol_id: Option<&str>,
    target_label: Option<&str>,
) -> GraphEdgeArtifact {
    GraphEdgeArtifact {
        source_stable_symbol_id: source_stable_symbol_id.to_owned(),
        target_stable_symbol_id: target_stable_symbol_id.map(str::to_owned),
        target_label: target_label.map(str::to_owned),
        import_path: None,
        relation: RelationKind::Calls,
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
        edge_kind: Some(GraphEdgeKind::Calls),
        bind_method: None,
    }
}

fn options(query: &str, mode: SearchMode) -> SearchOptions {
    SearchOptions {
        query: query.to_owned(),
        mode,
        filters: SearchFilters::default(),
        limit: 200,
    }
}

fn caller_records(records: Vec<OwnedCallerRecord>) -> Vec<(String, String, Option<String>, bool)> {
    let mut rows = records
        .into_iter()
        .map(|record| match record {
            OwnedCallerRecord::Resolved { caller, edge } => (
                caller.stable_symbol_id,
                edge.source_stable_symbol_id,
                edge.target_stable_symbol_id,
                true,
            ),
            OwnedCallerRecord::Unresolved {
                caller,
                edge,
                target_label,
            } => (
                caller.stable_symbol_id,
                edge.source_stable_symbol_id,
                Some(target_label),
                false,
            ),
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn callee_records(records: Vec<OwnedCalleeRecord>) -> Vec<(String, String, Option<String>, bool)> {
    let mut rows = records
        .into_iter()
        .map(|record| match record {
            OwnedCalleeRecord::Resolved { symbol, edge } => (
                symbol.stable_symbol_id,
                edge.source_stable_symbol_id,
                edge.target_stable_symbol_id,
                true,
            ),
            OwnedCalleeRecord::Unresolved { edge, target_label } => (
                target_label.clone(),
                edge.source_stable_symbol_id,
                Some(target_label),
                false,
            ),
        })
        .collect::<Vec<_>>();
    rows.sort();
    rows
}
