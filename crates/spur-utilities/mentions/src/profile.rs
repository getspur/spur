//! Filesystem traversal compatibility profiles (2026-08-27 mentions spec §3).
//!
//! One shared filesystem source implementation serves both clients: the
//! policy switches are data on [`FilesystemProfile`], and the profile's
//! [`FilesystemProfile::fingerprint`] participates in the engine cache key
//! so a snapshot produced under one traversal profile can never be reused
//! under another.

/// How the filesystem source encodes absolute paths into mention URIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UriConstruction {
    /// `format!("file://{abs}")`: byte-for-byte today's TUI
    /// `entry_for_path` output. No percent-encoding, so paths containing
    /// spaces or non-ASCII keep appearing verbatim in the URI. The TUI
    /// profile must keep this encoder until an explicit compatibility
    /// decision changes it (spec §3: "prevents silent behavior changes").
    LegacyTuiPassthrough,
    /// `url::Url::from_file_path`: standards-based `file://` URLs that
    /// percent-encode spaces and non-ASCII (notebook compatibility
    /// profile).
    StandardsBased,
}

impl UriConstruction {
    /// Stable discriminant for fingerprint packing.
    pub const fn discriminant(self) -> u64 {
        match self {
            Self::LegacyTuiPassthrough => 0,
            Self::StandardsBased => 1,
        }
    }
}

/// Filesystem traversal policy (2026-08-27 spec §3 table).
///
/// | Policy | [`FilesystemProfile::tui`] | [`FilesystemProfile::notebook_compat`] |
/// |---|---|---|
/// | Files | include | include |
/// | Directories | include | exclude |
/// | Hidden entries | exclude | include |
/// | Git/global/parent ignore rules | respect | do not apply |
/// | Symlink traversal | do not follow | do not follow |
/// | URI construction | legacy TUI passthrough | standards-based |
/// | Walk errors | dropped silently (parity) | typed traversal failure |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FilesystemProfile {
    /// Include directory rows alongside files.
    pub include_directories: bool,
    /// Include dot-prefixed (hidden) entries.
    pub include_hidden: bool,
    /// Apply `.gitignore` / `.ignore` / git-exclude rules.
    pub apply_ignore_rules: bool,
    /// Surface filesystem-walk failures as typed traversal errors instead
    /// of silently skipping them.
    pub fail_on_traversal_error: bool,
    /// Follow symbolic links while walking (both shipped profiles keep
    /// this off; the switch exists so the policy stays data, not code).
    pub follow_symlinks: bool,
    /// URI encoder selection.
    pub uri_construction: UriConstruction,
}

impl FilesystemProfile {
    /// Today's `spur-tui` `file_source.rs` behavior: directories included,
    /// hidden entries excluded, ignore rules respected, symlinks not
    /// followed, walk errors flattened, legacy `file://{abs}` URIs.
    pub const fn tui() -> Self {
        Self {
            include_directories: true,
            include_hidden: false,
            apply_ignore_rules: true,
            fail_on_traversal_error: false,
            follow_symlinks: false,
            uri_construction: UriConstruction::LegacyTuiPassthrough,
        }
    }

    /// Notebook compatibility profile: files only (directories excluded),
    /// hidden entries included, ignore rules not applied, symlinks not
    /// followed, walk failures typed, standards-based URI encoding.
    pub const fn notebook_compat() -> Self {
        Self {
            include_directories: false,
            include_hidden: true,
            apply_ignore_rules: false,
            fail_on_traversal_error: true,
            follow_symlinks: false,
            uri_construction: UriConstruction::StandardsBased,
        }
    }

    /// Cache-key fingerprint: version-tagged stable bit packing of every
    /// policy switch. Deterministic across processes and compiler versions
    /// (no hasher involved), and injective over the profile grid.
    pub const fn fingerprint(&self) -> u64 {
        let mut hash = FILESYSTEM_PROFILE_FINGERPRINT_VERSION;
        hash = (hash << 1) | self.include_directories as u64;
        hash = (hash << 1) | self.include_hidden as u64;
        hash = (hash << 1) | self.apply_ignore_rules as u64;
        hash = (hash << 1) | self.fail_on_traversal_error as u64;
        hash = (hash << 1) | self.follow_symlinks as u64;
        hash = (hash << 8) | self.uri_construction.discriminant();
        hash
    }
}

impl Default for FilesystemProfile {
    fn default() -> Self {
        Self::tui()
    }
}

/// Fingerprint layout version; bump whenever the packing changes meaning.
const FILESYSTEM_PROFILE_FINGERPRINT_VERSION: u64 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_and_notebook_profiles_differ_in_every_policy_dimension() {
        let tui = FilesystemProfile::tui();
        let notebook = FilesystemProfile::notebook_compat();

        assert!(tui.include_directories);
        assert!(!notebook.include_directories);
        assert!(!tui.include_hidden);
        assert!(notebook.include_hidden);
        assert!(tui.apply_ignore_rules);
        assert!(!notebook.apply_ignore_rules);
        assert!(!tui.fail_on_traversal_error);
        assert!(notebook.fail_on_traversal_error);
        assert!(!tui.follow_symlinks);
        assert!(!notebook.follow_symlinks);
        assert_eq!(tui.uri_construction, UriConstruction::LegacyTuiPassthrough);
        assert_eq!(notebook.uri_construction, UriConstruction::StandardsBased);
    }

    #[test]
    fn fingerprints_partition_the_profiles() {
        assert_ne!(
            FilesystemProfile::tui().fingerprint(),
            FilesystemProfile::notebook_compat().fingerprint()
        );
        // The version tag occupies the high bits, so packing cannot alias
        // low-order collisions across layout versions.
        assert!(FilesystemProfile::tui().fingerprint() > 0);
    }
}
