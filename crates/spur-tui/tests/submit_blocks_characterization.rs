//! sud-m0 characterization: pins the ACP `ContentBlock`s that
//! `spur_tui::commands::submit_router::{route, route_with_caps}` produce for
//! text carrying file / worker / datasource / code mentions, plus the
//! `blocks_preview` / `flatten_prompt_block` outputs, through public paths
//! only. Send decisions only — the `Local` variant is deliberately not
//! asserted (sud-c1 reshapes it). Fixtures live in tempdirs; no network.

use std::path::{Path, PathBuf};

use spur_acp::{
    AgentCapabilities, ContentBlock, EmbeddedResource, EmbeddedResourceResource, ResourceLink,
    SpurAgentCaps, TextContent, TextResourceContents,
};
use spur_graph::compute_anchor_hash;
use spur_tui::commands::submit_router::{
    assemble_blocks_with_code_mentions, blocks_preview, blocks_to_text, flatten_prompt_block,
    route, route_with_caps, SubmitDecision,
};
use spur_tui::commands::CommandRegistry;
use spur_tui::components::input_bar::{ProtectedRange, RangeKind};
use spur_tui::mentions::{CompletionScope, MentionKind, MentionRegistry};

const TEST_GRAPH_INDEX_VERSION: &str = "fixture-2026-05-11";

fn caps_with_prompt(image: bool, embedded_context: bool) -> SpurAgentCaps {
    let mut agent = AgentCapabilities::default();
    agent.prompt_capabilities.image = image;
    agent.prompt_capabilities.embedded_context = embedded_context;
    SpurAgentCaps {
        agent,
        modes: None,
        config_options: Vec::new(),
        agent_kind: spur_acp::AgentKind::Generic,
        grok_display: None,
        kiro_display: None,
        capability_evidence: None,
    }
}

/// Build one `RangeKind::Atom` mention covering `[start, start+atom.len())`.
fn atom_at(start: usize, atom: &str, uri: &str, name: &str) -> ProtectedRange {
    ProtectedRange {
        start,
        end: start + atom.len(),
        kind: RangeKind::Atom,
        uri: uri.to_string(),
        name: name.to_string(),
    }
}

fn send_blocks(decision: SubmitDecision) -> Vec<ContentBlock> {
    match decision {
        SubmitDecision::Send { blocks, interrupt } => {
            assert!(!interrupt, "Send fixture must not carry interrupt");
            blocks
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

fn text_of(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::Text(t) => t.text.as_str(),
        other => panic!("expected Text block, got {other:?}"),
    }
}

// ------------------------------------------------------------- plain text

#[test]
fn plain_text_routes_to_single_text_block() {
    let reg = CommandRegistry::new();
    for decision in [
        route("hello world", &[], &[], &reg, false),
        route_with_caps("hello world", &[], &[], &reg, false, None),
    ] {
        let blocks = send_blocks(decision);
        assert_eq!(blocks.len(), 1);
        assert_eq!(text_of(&blocks[0]), "hello world");
    }

    match route("!stop now", &[], &[], &reg, true) {
        SubmitDecision::Send { blocks, interrupt } => {
            assert!(interrupt);
            assert_eq!(text_of(&blocks[0]), "!stop now");
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn unknown_slash_command_falls_through_to_send() {
    let reg = CommandRegistry::new();
    let blocks = send_blocks(route(
        "/definitely-not-a-command arg",
        &[],
        &[],
        &reg,
        false,
    ));
    assert_eq!(blocks.len(), 1);
    assert_eq!(text_of(&blocks[0]), "/definitely-not-a-command arg");
}

// ---------------------------------------------------------- file mentions

#[test]
fn file_mention_routes_to_resource_link_without_caps() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn hello() {}\n").unwrap();
    let uri = format!("file://{}", tmp.path().join("lib.rs").display());
    let text = "review @lib.rs now";
    let ranges = [atom_at("review ".len(), "@lib.rs", &uri, "lib.rs")];

    let reg = CommandRegistry::new();
    let blocks = send_blocks(route(&text, &ranges, &[], &reg, false));
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert_eq!(text_of(&blocks[0]), "review ");
    match &blocks[1] {
        ContentBlock::ResourceLink(link) => {
            assert_eq!(link.name, "lib.rs");
            assert_eq!(link.uri, uri);
        }
        other => panic!("expected ResourceLink, got {other:?}"),
    }
    assert_eq!(text_of(&blocks[2]), " now");

    // The caps-unaware overload is route_with_caps(.., None).
    let with_caps = send_blocks(route_with_caps(&text, &ranges, &[], &reg, false, None));
    assert_eq!(with_caps.len(), 3, "{with_caps:?}");
    assert!(matches!(&with_caps[1], ContentBlock::ResourceLink(_)));
}

#[test]
fn file_mention_embeds_when_caps_advertise_embedded_context() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn hello() {}\n").unwrap();
    let uri = format!("file://{}", tmp.path().join("lib.rs").display());
    let text = "review @lib.rs now";
    let ranges = [atom_at("review ".len(), "@lib.rs", &uri, "lib.rs")];

    let reg = CommandRegistry::new();
    let caps = caps_with_prompt(false, true);
    let blocks = send_blocks(route_with_caps(
        &text,
        &ranges,
        &[],
        &reg,
        false,
        Some(&caps),
    ));

    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert_eq!(text_of(&blocks[0]), "review ");
    match &blocks[1] {
        ContentBlock::Resource(resource) => match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(contents) => {
                assert_eq!(contents.uri, uri);
                assert_eq!(contents.text, "fn hello() {}\n");
                assert_eq!(contents.mime_type.as_deref(), Some("text/x-rust"));
            }
            other => panic!("expected text resource, got {other:?}"),
        },
        other => panic!("expected Resource, got {other:?}"),
    }
    assert_eq!(text_of(&blocks[2]), " now");
    assert!(
        !blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ResourceLink(_))),
        "embed replaces the ResourceLink: {blocks:?}"
    );
}

#[test]
fn file_mention_embed_does_not_decode_percent_encoded_uri() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("with space.rs"), "fn s() {}\n").unwrap();
    let raw_uri = format!("file://{}", tmp.path().join("with space.rs").display());
    let encoded_uri = raw_uri.replace("with space.rs", "with%20space.rs");
    let caps = caps_with_prompt(false, true);
    let reg = CommandRegistry::new();

    for (label, uri) in [("raw", raw_uri), ("encoded", encoded_uri)] {
        let text = "open @with space.rs now";
        let ranges = [atom_at(
            "open ".len(),
            "@with space.rs",
            &uri,
            "with space.rs",
        )];
        let blocks = send_blocks(route_with_caps(
            &text,
            &ranges,
            &[],
            &reg,
            false,
            Some(&caps),
        ));
        match &blocks[1] {
            ContentBlock::Resource(_) => {
                assert_eq!(label, "raw", "encoded URI must not embed: {blocks:?}");
            }
            ContentBlock::ResourceLink(link) => {
                assert_eq!(label, "encoded", "raw URI must embed: {blocks:?}");
                assert_eq!(link.uri, uri);
            }
            other => panic!("unexpected block for {label}: {other:?}"),
        }
    }
}

#[test]
fn file_mention_embed_skips_oversized_file() {
    let tmp = tempfile::tempdir().unwrap();
    // PER_PROMPT_CAP_BYTES = 32 KiB; exceed it.
    std::fs::write(tmp.path().join("big.rs"), "x".repeat(33 * 1024)).unwrap();
    let uri = format!("file://{}", tmp.path().join("big.rs").display());
    let text = "@big.rs";
    let ranges = [atom_at(0, "@big.rs", &uri, "big.rs")];

    let reg = CommandRegistry::new();
    let caps = caps_with_prompt(false, true);
    let blocks = send_blocks(route_with_caps(
        &text,
        &ranges,
        &[],
        &reg,
        false,
        Some(&caps),
    ));
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    match &blocks[0] {
        ContentBlock::ResourceLink(link) => {
            assert_eq!(link.name, "big.rs");
            assert_eq!(link.uri, uri);
        }
        other => panic!("oversized embed must fall back to ResourceLink, got {other:?}"),
    }
}

// ------------------------------------------------- worker/datasource rows

#[test]
fn worker_and_datasource_mentions_route_to_resource_links() {
    let reg = CommandRegistry::new();

    // Composed worker atom: the name carries the composed slot summary, the
    // uri the query-parameterized worker URI.
    let worker_atom = "@worker:codex agent=a model=m effort=e";
    let worker_text = format!("{worker_atom} please");
    let worker_uri = "worker://codex?agent=a&model=m&effort=e";
    let ranges = [atom_at(
        0,
        worker_atom,
        worker_uri,
        worker_atom.trim_start_matches('@'),
    )];
    let blocks = send_blocks(route(&worker_text, &ranges, &[], &reg, false));
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    match &blocks[0] {
        ContentBlock::ResourceLink(link) => {
            assert_eq!(link.name, "worker:codex agent=a model=m effort=e");
            assert_eq!(link.uri, worker_uri);
        }
        other => panic!("expected worker ResourceLink, got {other:?}"),
    }
    assert_eq!(text_of(&blocks[1]), " please");

    let ds_text = "load @sales now";
    let ranges = [atom_at(
        "load ".len(),
        "@sales",
        "datasource://sales",
        "sales",
    )];
    let blocks = send_blocks(route(ds_text, &ranges, &[], &reg, false));
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    match &blocks[1] {
        ContentBlock::ResourceLink(link) => {
            assert_eq!(link.name, "sales");
            assert_eq!(link.uri, "datasource://sales");
        }
        other => panic!("expected datasource ResourceLink, got {other:?}"),
    }
}

// ----------------------------------------------------------- code mentions

#[test]
fn code_mention_through_route_emits_warning_without_payload_lookup() {
    let reg = CommandRegistry::new();
    let uri = "graph://symbol/symbol-config-struct";
    let atom = "@module config::Config";
    let text = "@module config::Config";
    let ranges = [atom_at(0, atom, uri, "module config::Config")];

    let blocks = send_blocks(route(&text, &ranges, &[], &reg, false));
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert_eq!(
        text_of(&blocks[0]),
        "MENTION_WARNING module config::Config\n\
         intended_uri:   graph://symbol/symbol-config-struct\n\
         failure_reason: payload_not_in_registry\n\
         replaced_with:  dropped\n"
    );
}

#[test]
fn code_mention_expands_and_appends_topology_hint_through_assemble() {
    let tmp = tempfile::tempdir().unwrap();
    let source = "impl App {\n    pub fn run(&self) {}\n}\n";
    let source_path = tmp.path().join("src/app.rs");
    std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    std::fs::write(&source_path, source).unwrap();
    let start = source.find("pub fn run").expect("run function");
    let slice = &source[start..source.len()];
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": TEST_GRAPH_INDEX_VERSION },
            "files": [
                { "stable_file_id": "file-app", "file_path": "src/app.rs" }
            ],
            "symbols": [
                {
                    "stable_symbol_id": "symbol-app-run",
                    "file_path": "src/app.rs",
                    "byte_range": [start, source.len()],
                    "line_range": [2, 3],
                    "entity_name": "run",
                    "qualified_name": "impl App::run",
                    "symbol_kind": "fn",
                    "anchor_hash": compute_anchor_hash(slice).to_string(),
                    "enclosing_scope": "App"
                }
            ]
        }),
    );

    let mut reg = MentionRegistry::for_direct_session().with_code_graph(graph_path);
    let symbol = reg
        .query(CompletionScope::PreSession, tmp.path(), "run", 10)
        .into_iter()
        .find(|hit| {
            hit.kind == MentionKind::CodeSymbol && hit.uri == "graph://symbol/symbol-app-run"
        })
        .expect("run symbol row");
    assert_eq!(symbol.atom_text.as_deref(), Some("@App::run"));

    let text = "@App::run";
    let ranges = [atom_at(0, "@App::run", &symbol.uri, "run")];
    let blocks = assemble_blocks_with_code_mentions(&text, &ranges, &[], tmp.path(), |uri| {
        reg.lookup_code_payload(uri)
    });
    assert_eq!(blocks.len(), 2, "expansion + topology hint: {blocks:?}");
    let expansion = text_of(&blocks[0]);
    assert!(expansion.contains("MENTION App::run\n"), "{expansion}");
    assert!(
        expansion.contains("qualified_name: impl App::run\n"),
        "{expansion}"
    );
    assert!(
        expansion.contains("id:      graph://symbol/symbol-app-run"),
        "{expansion}"
    );
    let hint = text_of(&blocks[1]);
    assert!(
        hint.contains(
            "topology_available_via_mcp_for_above_symbols: pass each MENTION's qualified_name OR path:line to code_callers / code_callees / code_subgraph(radius=1); use code_resolve for ambiguous names"
        ),
        "{hint}"
    );

    // Both expansion blocks flatten into the preview echo.
    assert_eq!(flatten_prompt_block(&blocks[0]).as_deref(), Some(expansion));
    assert_eq!(flatten_prompt_block(&blocks[1]).as_deref(), Some(hint));
    let preview = blocks_preview(&blocks);
    assert!(preview.contains("MENTION App::run"), "{preview}");
    assert!(preview.ends_with(hint), "{preview}");
}

// ------------------------------------------- flatten_prompt_block + preview

#[test]
fn flatten_prompt_block_matrix() {
    // Plain text round-trips.
    assert_eq!(
        flatten_prompt_block(&ContentBlock::Text(TextContent::new("plain"))),
        Some("plain".to_string())
    );
    // [UI hint] framing is agent-only and never echoes locally.
    assert_eq!(
        flatten_prompt_block(&ContentBlock::Text(TextContent::new(
            "[UI hint] User-suggested workers for delegation this turn: claude-code."
        ))),
        None
    );
    // ResourceLinks flatten to @<name>.
    assert_eq!(
        flatten_prompt_block(&ContentBlock::ResourceLink(ResourceLink::new(
            "claude-code",
            "worker://claude-code"
        ))),
        Some("@claude-code".to_string())
    );
    // Embedded resources flatten to the last URI path segment.
    assert_eq!(
        flatten_prompt_block(&ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
                "fn main() {}",
                "file:///tmp/proj/src/lib.rs"
            ))
        ))),
        Some("@lib.rs".to_string())
    );
    assert_eq!(
        flatten_prompt_block(&ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
                "rows",
                "datasource://sales"
            ))
        ))),
        Some("@sales".to_string())
    );
}

#[test]
fn blocks_preview_concatenates_and_skips_ui_hint() {
    let blocks = vec![
        ContentBlock::Text(TextContent::new(
            "[UI hint] User-suggested workers for delegation this turn: claude-code \
             (preference, not override).",
        )),
        ContentBlock::ResourceLink(ResourceLink::new("claude-code", "worker://claude-code")),
        ContentBlock::Text(TextContent::new(" please")),
    ];
    assert_eq!(blocks_preview(&blocks), "@claude-code please");
    // blocks_to_text is currently identical to blocks_preview.
    assert_eq!(blocks_to_text(&blocks), blocks_preview(&blocks));
}

// ---------------------------------------------------------- graph fixture

fn write_graph_fixture(root: &Path, value: serde_json::Value) -> PathBuf {
    let mut artifact: spur_graph::GraphIndexArtifact =
        serde_json::from_value(value).expect("graph fixture json must match artifact shape");
    let n_files = artifact.files.len();
    artifact.file_node_ids = (0..n_files as u64).map(spur_graph::NodeId).collect();
    artifact.symbol_node_ids = (0..artifact.symbols.len() as u64)
        .map(|i| spur_graph::NodeId(n_files as u64 + i))
        .collect();
    spur_graph::write_artifact_parquet(
        &artifact,
        root,
        spur_graph::WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet graph fixture");
    root.to_path_buf()
}
