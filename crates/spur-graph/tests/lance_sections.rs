use std::fs;

use arrow_array::{LargeStringArray, UInt32Array};
use futures::TryStreamExt as _;
use lancedb::query::{ExecutableQuery as _, QueryBase as _, Select};
use spur_graph::store::lance_sections::{
    write_sections_dataset, SECTIONS_DATASET_DIR, SECTIONS_TABLE,
};
use spur_graph::{artifact_from_facts, build_facts};

#[tokio::test]
async fn writes_markdown_sections_dataset_with_body_text() {
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
