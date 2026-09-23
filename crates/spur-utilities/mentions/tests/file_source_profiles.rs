//! sud-m2a filesystem-profile contract tests: the TUI-compatible profile
//! must match today's `spur-tui/src/mentions/file_source.rs` byte for byte
//! (walker flags, hidden/ignore handling, directory display, legacy
//! `file://{abs}` URIs), while the notebook compatibility profile follows
//! the 2026-08-27 spec §3 table.

mod common;

use std::collections::BTreeSet;

use common::{legacy_file_uri, TempTree};
use spur_mentions::{
    FileMentionSource, FilesystemProfile, MentionKind, MentionSource as _, SourceContext,
    SourceSnapshot, UriConstruction,
};

fn build(profile: FilesystemProfile, root: &std::path::Path) -> SourceSnapshot {
    FileMentionSource::new()
        .build(
            root,
            &SourceContext {
                filesystem_profile: profile,
            },
        )
        .expect("filesystem build succeeds")
}

/// (kind, display, uri) summary in display order.
fn rows(snapshot: &SourceSnapshot) -> Vec<(String, String, String)> {
    let mut rows: Vec<(String, String, String)> = snapshot
        .entries
        .iter()
        .map(|e| {
            let kind = match &e.kind {
                MentionKind::File => "file",
                MentionKind::Directory => "dir",
                MentionKind::CodeFile | MentionKind::CodeSymbol => "code",
                MentionKind::Custom(name) => name.as_ref(),
            };
            (kind.to_owned(), e.display.clone(), e.uri.clone())
        })
        .collect();
    // Compare in display order (profile-agnostic view of the snapshot).
    rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
    rows
}

fn sample_tree() -> TempTree {
    let tree = TempTree::new("profiles");
    tree.git_dir();
    tree.file(".gitignore", "ignored.txt\n");
    tree.file("ignored.txt", "ignored");
    tree.file(".dotfile", "hidden");
    tree.file(".hidden/secret.txt", "hidden");
    tree.file("a.rs", "a");
    tree.file("sub/b.rs", "b");
    tree.file("sp ace.txt", "space");
    tree.file("café.txt", "cafe");
    tree
}

#[test]
fn tui_profile_matches_todays_file_source() {
    let tree = sample_tree();
    let snapshot = build(FilesystemProfile::tui(), tree.root());
    let root = tree.root();
    let got = rows(&snapshot);

    let expected = vec![
        ("file", "a.rs", legacy_file_uri(&root.join("a.rs"))),
        ("file", "café.txt", legacy_file_uri(&root.join("café.txt"))),
        (
            "file",
            "sp ace.txt",
            legacy_file_uri(&root.join("sp ace.txt")),
        ),
        ("dir", "sub/", legacy_file_uri(&root.join("sub"))),
        ("file", "sub/b.rs", legacy_file_uri(&root.join("sub/b.rs"))),
    ]
    .into_iter()
    .map(|(k, d, u)| (k.to_owned(), d.to_owned(), u))
    .collect::<Vec<_>>();

    assert_eq!(got, expected);
}

#[test]
fn tui_profile_mints_sequential_unique_ids() {
    let tree = sample_tree();
    let snapshot = build(FilesystemProfile::tui(), tree.root());
    let ids: BTreeSet<u64> = snapshot.entries.iter().map(|e| e.id.as_u64()).collect();
    assert_eq!(ids.len(), snapshot.entries.len());
    assert_eq!(ids.first().copied(), Some(0));
    assert_eq!(ids.last().copied(), Some(snapshot.entries.len() as u64 - 1));
}

#[test]
#[cfg(unix)]
fn tui_profile_labels_symlinked_directories_without_traversing_them() {
    let tree = TempTree::new("profiles-symlink");
    tree.file("sub/b.rs", "");
    tree.symlink_dir("sub", "link");
    let snapshot = build(FilesystemProfile::tui(), tree.root());

    let displays: Vec<&str> = snapshot
        .entries
        .iter()
        .map(|e| e.display.as_str())
        .collect();
    // The symlink itself appears as a directory row (today's behavior: the
    // walker does not follow it, but `is_dir()` labels the link target).
    assert!(displays.contains(&"link/"));
    // Nothing under the symlink leaks: no `link/…` children.
    assert!(snapshot
        .entries
        .iter()
        .all(|e| e.display == "link/" || !e.display.starts_with("link/")));
    // The real target is traversed exactly once.
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|e| e.display.ends_with("b.rs"))
            .count(),
        1
    );
}

#[test]
fn notebook_profile_includes_hidden_and_ignored_files_without_directories() {
    let tree = sample_tree();
    let snapshot = build(FilesystemProfile::notebook_compat(), tree.root());
    let got = rows(&snapshot);

    let encoded = |rel: &str| {
        url::Url::from_file_path(tree.root().join(rel))
            .expect("notebook URI encodes")
            .to_string()
    };
    let expected = vec![
        (".dotfile", encoded(".dotfile")),
        (".gitignore", encoded(".gitignore")),
        (".hidden/secret.txt", encoded(".hidden/secret.txt")),
        ("a.rs", encoded("a.rs")),
        ("café.txt", encoded("café.txt")),
        ("ignored.txt", encoded("ignored.txt")),
        ("sp ace.txt", encoded("sp ace.txt")),
        ("sub/b.rs", encoded("sub/b.rs")),
    ]
    .into_iter()
    .map(|(d, u)| ("file".to_owned(), d.to_owned(), u))
    .collect::<Vec<_>>();

    assert_eq!(got, expected);
    // Standards-based encoding is real: spaces and non-ASCII percent-encode.
    assert!(got.iter().any(|(_, _, u)| u.contains("%20")));
    assert!(got.iter().any(|(_, _, u)| u.contains("caf%C3%A9.txt")));
}

#[test]
fn both_profiles_skip_the_root_itself() {
    let tree = sample_tree();
    for profile in [
        FilesystemProfile::tui(),
        FilesystemProfile::notebook_compat(),
    ] {
        let snapshot = build(profile, tree.root());
        assert!(snapshot
            .entries
            .iter()
            .all(|e| !e.display.is_empty() && e.uri != legacy_file_uri(tree.root())));
    }
}

#[test]
fn tui_profile_drops_walk_errors_silently_for_parity() {
    let missing = std::env::temp_dir().join("spur-mentions-missing-walk-root");
    let snapshot = FileMentionSource::new()
        .build(
            &missing,
            &SourceContext {
                filesystem_profile: FilesystemProfile::tui(),
            },
        )
        .expect("today's walker flattens traversal errors");
    assert!(snapshot.entries.is_empty());
}

#[test]
fn notebook_profile_surfaces_walk_errors_as_typed_traversal_failures() {
    let missing = std::env::temp_dir().join("spur-mentions-missing-walk-root");
    let error = FileMentionSource::new()
        .build(
            &missing,
            &SourceContext {
                filesystem_profile: FilesystemProfile::notebook_compat(),
            },
        )
        .expect_err("notebook profile types walk failures");
    assert_eq!(error.kind, spur_mentions::SourceBuildErrorKind::Traversal);
}

#[test]
#[cfg(unix)]
fn non_utf8_paths_are_skipped_not_panicked() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let tree = TempTree::new("profiles-non-utf8");
    // APFS and some filesystems refuse non-UTF-8 names; where creation
    // fails there is nothing to skip, so the invariant holds trivially.
    match std::fs::File::create(tree.root().join(OsStr::from_bytes(b"bad\xff.rs"))) {
        Ok(file) => drop(file),
        Err(_) => return,
    }

    for profile in [
        FilesystemProfile::tui(),
        FilesystemProfile::notebook_compat(),
    ] {
        let snapshot = build(profile, tree.root());
        assert!(snapshot.entries.is_empty(), "{profile:?}");
    }
}

#[test]
fn profiles_have_distinct_stable_fingerprints() {
    let tui = FilesystemProfile::tui();
    let notebook = FilesystemProfile::notebook_compat();

    assert_ne!(tui.fingerprint(), notebook.fingerprint());
    // Stable across calls and constructions.
    assert_eq!(tui.fingerprint(), FilesystemProfile::tui().fingerprint());
    assert_eq!(
        notebook.fingerprint(),
        FilesystemProfile::notebook_compat().fingerprint()
    );
    // URI encoder selection also partitions the key.
    let mut standards_based = tui;
    standards_based.uri_construction = UriConstruction::StandardsBased;
    assert_ne!(tui.fingerprint(), standards_based.fingerprint());
}

#[test]
fn fingerprint_is_injective_over_the_bool_grid() {
    let mut fingerprints = std::collections::BTreeSet::new();
    let mut total = 0usize;
    for include_directories in [false, true] {
        for include_hidden in [false, true] {
            for apply_ignore_rules in [false, true] {
                for fail_on_traversal_error in [false, true] {
                    let profile = FilesystemProfile {
                        include_directories,
                        include_hidden,
                        apply_ignore_rules,
                        fail_on_traversal_error,
                        follow_symlinks: false,
                        uri_construction: UriConstruction::LegacyTuiPassthrough,
                    };
                    fingerprints.insert(profile.fingerprint());
                    total += 1;
                }
            }
        }
    }
    assert_eq!(fingerprints.len(), total, "fingerprint collisions");
}

#[test]
fn source_context_defaults_to_the_tui_profile() {
    assert_eq!(
        SourceContext::default().filesystem_profile,
        FilesystemProfile::tui()
    );
}
