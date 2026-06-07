use std::fs;

use arrow_array::{Array as _, LargeStringArray, StringArray, UInt32Array};
use futures::TryStreamExt as _;
use lancedb::query::{ExecutableQuery as _, QueryBase as _, Select};
use spur_graph::store::lance_sections::{
    write_sections_dataset, SECTIONS_DATASET_DIR, SECTIONS_TABLE,
};
use spur_graph::{
    artifact_from_facts, build_facts, GraphFileArtifact, GraphFileManifestEntry,
    GraphIndexArtifact, GraphIndexHeader, GraphSymbolArtifact,
};

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[tokio::test]
async fn lance_sections_writes_markdown_sections_dataset_with_body_text() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    fs::write(
        root.join("docs/guide.md"),
        "# Guide\n\nIntro body.\n\n## Install\n\nInstall body line.\n\n## Use\n\nUse body line.\n",
    )
    .expect("write guide");
    fs::write(
        root.join("docs/notes.md"),
        "plain note without headings\nsecond line\n",
    )
    .expect("write notes");
    fs::write(root.join("docs/other.md"), "# Other\n\nOther body.\n").expect("write other");

    let facts = build_facts(&root, None).expect("build facts").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let out_dir = tempdir.path().join("artifact");

    write_sections_dataset(&artifact, &root, &out_dir).expect("write sections sidecar");

    let dataset_dir = out_dir.join(SECTIONS_DATASET_DIR);
    assert!(dataset_dir.exists(), "sections.lancedb should exist");

    let db = lancedb::connect(dataset_dir.to_str().expect("dataset path"))
        .execute()
        .await
        .expect("connect lancedb");
    let table = db
        .open_table(SECTIONS_TABLE)
        .execute()
        .await
        .expect("open table");
    assert_eq!(table.count_rows(None).await.expect("count rows"), 5);

    let batches = table
        .query()
        .only_if("qualified_name = 'Guide::Install'")
        .select(Select::columns(&["body_text", "child_count"]))
        .limit(1)
        .execute()
        .await
        .expect("query install")
        .try_collect::<Vec<_>>()
        .await
        .expect("collect install");
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].num_rows(), 1);
    let body_text = batches[0]
        .column_by_name("body_text")
        .expect("body_text column")
        .as_any()
        .downcast_ref::<LargeStringArray>()
        .expect("body_text large string");
    assert!(
        body_text.value(0).contains("Install body line."),
        "section body_text should include the widened section body: {:?}",
        body_text.value(0)
    );
    let child_count = batches[0]
        .column_by_name("child_count")
        .expect("child_count column")
        .as_any()
        .downcast_ref::<UInt32Array>()
        .expect("child_count u32");
    assert_eq!(child_count.value(0), 0);
}

#[tokio::test]
async fn lance_sections_skip_section_embeddings_writes_null_vectors() {
    let _env = EnvGuard::set("SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS", "1");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    fs::write(
        root.join("docs/guide.md"),
        "# Guide\n\n## Install\n\nInstall body.\n",
    )
    .expect("write guide");

    let facts = build_facts(&root, None).expect("build facts").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let out_dir = tempdir.path().join("artifact");

    write_sections_dataset(&artifact, &root, &out_dir).expect("write sections sidecar");

    let db = lancedb::connect(
        out_dir
            .join(SECTIONS_DATASET_DIR)
            .to_str()
            .expect("dataset path"),
    )
    .execute()
    .await
    .expect("connect lancedb");
    let table = db
        .open_table(SECTIONS_TABLE)
        .execute()
        .await
        .expect("open table");

    assert_eq!(
        table
            .count_rows(Some("vector IS NOT NULL".to_owned()))
            .await
            .expect("count vector rows"),
        0
    );
}

#[tokio::test]
async fn lance_sections_streams_small_write_batches_without_vectors() {
    let _skip = EnvGuard::set("SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS", "1");
    let _write_batch = EnvGuard::set("SPUR_GRAPH_SECTION_WRITE_BATCH_SIZE", "2");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    fs::write(
        root.join("docs/guide.md"),
        "# Guide\n\n## One\n\nBody one.\n\n## Two\n\nBody two.\n\n## Three\n\nBody three.\n\n## Four\n\nBody four.\n\n## Five\n\nBody five.\n",
    )
    .expect("write guide");

    let facts = build_facts(&root, None).expect("build facts").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let section_symbol_count = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.symbol_kind == "section")
        .count();
    assert!(
        section_symbol_count > 2,
        "test fixture should cross the configured write batch boundary"
    );
    let out_dir = tempdir.path().join("artifact");

    write_sections_dataset(&artifact, &root, &out_dir).expect("write sections sidecar");

    let db = lancedb::connect(
        out_dir
            .join(SECTIONS_DATASET_DIR)
            .to_str()
            .expect("dataset path"),
    )
    .execute()
    .await
    .expect("connect lancedb");
    let table = db
        .open_table(SECTIONS_TABLE)
        .execute()
        .await
        .expect("open table");

    assert_eq!(
        table.count_rows(None).await.expect("count rows"),
        section_symbol_count
    );
    assert_eq!(
        table
            .count_rows(Some("vector IS NOT NULL".to_owned()))
            .await
            .expect("count vector rows"),
        0
    );
}

#[tokio::test]
async fn lance_sections_skips_non_utf8_markdown_files() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("mkdir docs");

    let good_source = "# Good\n\nValid body.\n";
    fs::write(root.join("docs/good.md"), good_source).expect("write good");
    fs::write(root.join("docs/bad.md"), b"# Bad\n\nInvalid \xFE body.\n").expect("write bad");

    let artifact = GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "test".to_owned(),
            content_hash_blake3: None,
        },
        manifest_version: "test".to_owned(),
        graph_content_hash: "test".to_owned(),
        file_manifests: vec![
            GraphFileManifestEntry {
                stable_file_id: "file-good".to_owned(),
                path: "docs/good.md".to_owned(),
                content_oid: "good".to_owned(),
                node_ids: Vec::new(),
            },
            GraphFileManifestEntry {
                stable_file_id: "file-bad".to_owned(),
                path: "docs/bad.md".to_owned(),
                content_oid: "bad".to_owned(),
                node_ids: Vec::new(),
            },
        ],
        files: vec![
            GraphFileArtifact {
                stable_file_id: "file-good".to_owned(),
                file_path: "docs/good.md".to_owned(),
            },
            GraphFileArtifact {
                stable_file_id: "file-bad".to_owned(),
                file_path: "docs/bad.md".to_owned(),
            },
        ],
        file_node_ids: Vec::new(),
        symbols: vec![
            GraphSymbolArtifact {
                stable_symbol_id: "section-good".to_owned(),
                file_path: "docs/good.md".to_owned(),
                byte_range: [0, good_source.len()],
                line_range: [1, 3],
                entity_name: "Good".to_owned(),
                qualified_name: "Good".to_owned(),
                symbol_kind: "section".to_owned(),
                anchor_hash: "anchor-good".to_owned(),
                enclosing_scope: None,
            },
            GraphSymbolArtifact {
                stable_symbol_id: "section-bad".to_owned(),
                file_path: "docs/bad.md".to_owned(),
                byte_range: [0, 24],
                line_range: [1, 3],
                entity_name: "Bad".to_owned(),
                qualified_name: "Bad".to_owned(),
                symbol_kind: "section".to_owned(),
                anchor_hash: "anchor-bad".to_owned(),
                enclosing_scope: None,
            },
        ],
        symbol_node_ids: Vec::new(),
        edges: Vec::new(),
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    };
    let out_dir = tempdir.path().join("artifact");

    write_sections_dataset(&artifact, &root, &out_dir).expect("write sections sidecar");

    let db = lancedb::connect(
        out_dir
            .join(SECTIONS_DATASET_DIR)
            .to_str()
            .expect("dataset path"),
    )
    .execute()
    .await
    .expect("connect lancedb");
    let table = db
        .open_table(SECTIONS_TABLE)
        .execute()
        .await
        .expect("open table");
    let batches = table
        .query()
        .select(Select::columns(&["file_path"]))
        .execute()
        .await
        .expect("query file paths")
        .try_collect::<Vec<_>>()
        .await
        .expect("collect file paths");

    let file_paths: Vec<_> = batches
        .iter()
        .flat_map(|batch| {
            let paths = batch
                .column_by_name("file_path")
                .expect("file_path column")
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("file_path string");
            (0..paths.len()).map(|index| paths.value(index).to_owned())
        })
        .collect();

    assert_eq!(file_paths, vec!["docs/good.md".to_owned()]);
    assert!(!file_paths.iter().any(|path| path == "docs/bad.md"));
}
