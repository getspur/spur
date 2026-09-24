//! Shared filesystem mention source (2026-08-27 mentions spec §3).
//!
//! One implementation serves both compatibility profiles: every traversal
//! switch (hidden entries, ignore rules, directories, symlink following,
//! URI encoding, walk-error policy) is data on the
//! [`FilesystemProfile`] carried by [`SourceContext`], so `build` needs no
//! per-frontend signature.
//!
//! The TUI-compatible profile is pinned byte-for-byte to today's
//! `spur-tui/src/mentions/file_source.rs`: same walker flags, same
//! directory display (`rel/`), same legacy `format!("file://{abs}")` URIs,
//! and the same silent flattening of walk errors.

use std::path::Path;

use ignore::WalkBuilder;
use url::Url;

use crate::entry::{
    MentionEntry, MentionId, MentionKind, MentionSource, SourceBuildError, SourceContext,
    SourceSnapshot,
};
use crate::profile::{FilesystemProfile, UriConstruction};

/// Walker-backed filesystem source producing neutral file-tree rows.
pub struct FileMentionSource;

impl FileMentionSource {
    /// Create the source. The traversal policy arrives per build through
    /// [`SourceContext`], not at construction.
    pub const fn new() -> Self {
        Self
    }
}

impl Default for FileMentionSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MentionSource for FileMentionSource {
    fn key(&self) -> &'static str {
        "file"
    }

    fn build(
        &mut self,
        root: &Path,
        context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError> {
        let profile = context.filesystem_profile;
        let walker = WalkBuilder::new(root)
            .follow_links(profile.follow_symlinks)
            .hidden(!profile.include_hidden)
            .git_ignore(profile.apply_ignore_rules)
            .git_exclude(profile.apply_ignore_rules)
            .ignore(profile.apply_ignore_rules)
            .build();
        let mut entries = Vec::new();
        let mut next_id = MentionId::new(0);
        for dent in walker {
            let dent = match dent {
                Ok(dent) => dent,
                Err(error) => {
                    if profile.fail_on_traversal_error {
                        // Redacted: the io error class only, never the path.
                        let reason = error
                            .io_error()
                            .map(|io| io.kind().to_string())
                            .unwrap_or_else(|| "walk error".to_owned());
                        return Err(SourceBuildError::traversal(format!(
                            "filesystem walk failed: {reason}"
                        )));
                    }
                    // TUI parity: today's walker flattens traversal errors.
                    continue;
                }
            };
            let path = dent.path();
            if path == root {
                continue;
            }
            let is_directory = path.is_dir();
            if is_directory && !profile.include_directories {
                continue;
            }
            if let Some(entry) = entry_for_path(root, path, profile, is_directory, next_id) {
                entries.push(entry);
                next_id = next_id.next();
            }
        }
        Ok(SourceSnapshot::new(entries, 0))
    }
}

/// Convert one absolute path under `root` into a neutral entry, following
/// the profile's URI encoder and display conventions. Returns `None` for
/// rows the profile cannot represent (non-UTF-8 paths).
fn entry_for_path(
    root: &Path,
    abs: &Path,
    profile: FilesystemProfile,
    is_directory: bool,
    id: MentionId,
) -> Option<MentionEntry> {
    let rel = abs.strip_prefix(root).ok()?;
    let rel_str = rel.to_str()?;
    let abs_str = abs.to_str()?;
    let kind = if is_directory {
        MentionKind::Directory
    } else {
        MentionKind::File
    };
    let display = if is_directory {
        format!("{rel_str}/")
    } else {
        rel_str.to_owned()
    };
    let uri = match profile.uri_construction {
        // Today's TUI formatting, unchanged: no percent-encoding, so a path
        // containing spaces or non-ASCII keeps appearing verbatim.
        UriConstruction::LegacyTuiPassthrough => format!("file://{abs_str}"),
        UriConstruction::StandardsBased => Url::from_file_path(abs).ok()?.to_string(),
    };
    Some(MentionEntry {
        id,
        kind,
        uri,
        display,
        secondary: None,
        search_text: None,
        insert_text: None,
    })
}
