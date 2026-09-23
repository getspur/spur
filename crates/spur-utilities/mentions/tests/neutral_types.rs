//! sud-m1 core-type contract tests for `spur-mentions`
//! (2026-08-27 mentions spec §2 "Public API and data model").
//!
//! These run in both feature configurations: `--no-default-features` is the
//! notebook's shape and must never see `spur-graph` or ACP surface, while the
//! workspace build compiles this crate with `code` on (feature unification).

use std::collections::HashMap;
use std::path::Path;

use spur_mentions::{
    MentionEntry, MentionId, MentionKind, MentionSource, SourceBuildError, SourceContext,
    SourceSnapshot,
};

fn entry(id: MentionId, kind: MentionKind, uri: &str, display: &str) -> MentionEntry {
    MentionEntry {
        id,
        kind,
        uri: uri.to_owned(),
        display: display.to_owned(),
        secondary: None,
        search_text: None,
        insert_text: None,
    }
}

#[test]
fn mention_id_is_a_copyable_sidecar_key() {
    let first = MentionId::new(7);
    assert_eq!(first.as_u64(), 7);
    assert_eq!(first, MentionId::new(7));
    assert_ne!(first, MentionId::new(8));
    assert!(first < first.next());
    assert_eq!(first.next(), MentionId::new(8));

    // The sidecar contract: `MentionId` is a hash key stable within one
    // source snapshot.
    let mut sidecar: HashMap<MentionId, &str> = HashMap::new();
    sidecar.insert(first, "worker");
    sidecar.insert(first.next(), "issue");
    assert_eq!(sidecar.len(), 2);
    assert_eq!(sidecar.get(&first), Some(&"worker"));
}

#[test]
fn mention_kind_covers_file_tree_and_caller_categories() {
    assert_eq!(MentionKind::File, MentionKind::File);
    let worker = MentionKind::Custom("worker".into());
    let issue = MentionKind::Custom("issue".into());
    assert_ne!(worker, issue);
    assert_eq!(worker, MentionKind::Custom("worker".into()));

    // The file-tree variants exist regardless of the `code` feature; only
    // `CodeMentionPayload` is feature-gated.
    let _ = (
        MentionKind::File,
        MentionKind::Directory,
        MentionKind::CodeFile,
        MentionKind::CodeSymbol,
    );
}

#[test]
fn neutral_entry_is_pure_ranking_and_insertion_data() {
    let mut neutral = entry(
        MentionId::new(1),
        MentionKind::File,
        "file:///tmp/a.rs",
        "src/a.rs",
    );
    neutral.secondary = Some("detail".into());
    neutral.search_text = Some("src/a.rs a.rs".into());
    neutral.insert_text = Some("@a.rs".into());

    assert_eq!(neutral.secondary.as_deref(), Some("detail"));
    assert_eq!(neutral.search_text.as_deref(), Some("src/a.rs a.rs"));
    assert_eq!(neutral.insert_text.as_deref(), Some("@a.rs"));

    let cloned = neutral.clone();
    assert_eq!(cloned, neutral);
}

#[test]
fn snapshot_carries_entries_and_a_monotonic_generation() {
    let cold = SourceSnapshot::new(
        vec![entry(
            MentionId::new(0),
            MentionKind::File,
            "file:///tmp/a",
            "a",
        )],
        0,
    );
    let rebuilt = SourceSnapshot::new(Vec::new(), cold.generation + 1);

    assert_eq!(cold.entries.len(), 1);
    assert_eq!(cold.entries[0].uri, "file:///tmp/a");
    assert!(rebuilt.generation > cold.generation);
    assert_eq!(cold, cold.clone());
}

struct FixtureSource {
    key: &'static str,
    fail: bool,
}

impl MentionSource for FixtureSource {
    fn key(&self) -> &str {
        self.key
    }

    fn build(
        &mut self,
        root: &Path,
        _context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError> {
        if self.fail {
            return Err(SourceBuildError::new(format!(
                "fixture cannot read {}",
                root.display()
            )));
        }
        Ok(SourceSnapshot::new(
            vec![entry(
                MentionId::new(0),
                MentionKind::Custom("worker".into()),
                "worker://claude",
                "worker:claude",
            )],
            3,
        ))
    }
}

#[test]
fn mention_source_trait_is_object_safe() {
    let mut source: Box<dyn MentionSource> = Box::new(FixtureSource {
        key: "fixture",
        fail: false,
    });
    assert_eq!(source.key(), "fixture");

    let snapshot = source
        .build(Path::new("/tmp"), &SourceContext::default())
        .expect("fixture build succeeds");
    assert_eq!(snapshot.generation, 3);
    assert_eq!(snapshot.entries[0].uri, "worker://claude");
}

#[test]
fn source_build_failures_are_typed_not_flattened() {
    let mut failing: Box<dyn MentionSource> = Box::new(FixtureSource {
        key: "fixture",
        fail: true,
    });
    let error = failing
        .build(Path::new("/tmp"), &SourceContext::default())
        .expect_err("fixture build must fail");

    assert_eq!(error, SourceBuildError::new("fixture cannot read /tmp"));
    assert_eq!(error.to_string(), "fixture cannot read /tmp");
    let boxed: Box<dyn std::error::Error> = Box::new(error);
    assert_eq!(boxed.to_string(), "fixture cannot read /tmp");
}

#[cfg(feature = "code")]
#[test]
fn code_payload_export_exists_only_with_the_code_feature() {
    // With `code` on, the spur-graph payload joins the crate surface. The
    // `--no-default-features` gate pins the opposite configuration: this
    // test compiles away and nothing graph-shaped leaks.
    fn assert_exported(payload: Option<spur_mentions::CodeMentionPayload>) -> usize {
        std::mem::size_of_val(&payload)
    }
    assert!(assert_exported(None) > 0);
}
