//! Skills installer: renders bundled+override skills into per-adapter
//! agent dirs, protects user hand-edits via an in-file marker + sha256.

use sha2::{Digest, Sha256};
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

/// SPUR-MANAGED marker embedded in every file the installer writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    pub version: u8,
    pub skill_id: String,
    pub sha256: String, // lowercase hex, 64 chars
}

impl Marker {
    /// Render the marker as a single line including the trailing newline.
    pub fn render(&self) -> String {
        format!(
            "<!-- SPUR-MANAGED v={} skill={} sha256={} -->\n",
            self.version, self.skill_id, self.sha256
        )
    }
}

static MARKER_RE: OnceLock<regex::Regex> = OnceLock::new();

fn marker_regex() -> &'static regex::Regex {
    MARKER_RE.get_or_init(|| {
        regex::Regex::new(
            r"^<!-- SPUR-MANAGED v=(\d+) skill=(\S+) sha256=([0-9a-f]{64}) -->$",
        )
        .expect("static regex")
    })
}

/// Parse a marker line (without trailing newline) into its components.
pub(crate) fn parse_marker(line: &str) -> Option<Marker> {
    let caps = marker_regex().captures(line)?;
    Some(Marker {
        version: caps.get(1)?.as_str().parse().ok()?,
        skill_id: caps.get(2)?.as_str().to_string(),
        sha256: caps.get(3)?.as_str().to_string(),
    })
}

/// Lowercase hex sha256 of the given bytes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

/// Why a target path was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// File exists, has no SPUR-MANAGED marker — treat as user-owned.
    NoMarker,
    /// File exists, has a marker, but body hash does not match marker's
    /// embedded hash — user edited since last install.
    UserEdited,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::NoMarker => write!(f, "user-owned (no marker)"),
            SkipReason::UserEdited => write!(f, "user-edited"),
        }
    }
}

/// Report of what the installer did in a single run.
#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub written: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, SkipReason)>,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "SpurPower skills: wrote {w}, unchanged {u}, skipped {s}",
            w = self.written.len(),
            u = self.unchanged.len(),
            s = self.skipped.len(),
        )?;
        for (p, reason) in &self.skipped {
            writeln!(f, "  skipped {} ({reason})", p.display())?;
        }
        Ok(())
    }
}

/// Error variant for any failure during install.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },

    #[error("invalid skill id `{id}`: {reason}")]
    InvalidSkillId { id: String, reason: String },
}

impl From<crate::skills::InvalidSkillId> for InstallError {
    fn from(e: crate::skills::InvalidSkillId) -> Self {
        InstallError::InvalidSkillId {
            id: e.id,
            reason: e.reason.to_string(),
        }
    }
}

/// Atomic write: write to a sibling tempfile, then rename(2) over the
/// target. Creates parent directories as needed.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| InstallError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| {
        InstallError::Io { path: parent.to_path_buf(), source }
    })?;
    use std::io::Write as _;
    tmp.write_all(bytes).map_err(|source| InstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(path).map_err(|e| InstallError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_roundtrip() {
        let m = Marker {
            version: 1,
            skill_id: "tdd".to_string(),
            sha256: "a".repeat(64),
        };
        let rendered = m.render();
        let line = rendered.trim_end_matches('\n');
        let parsed = parse_marker(line).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn parse_marker_rejects_garbage() {
        assert!(parse_marker("not a marker").is_none());
        assert!(parse_marker("<!-- SPUR-MANAGED v=1 skill=x -->").is_none()); // no sha
        assert!(parse_marker(
            "<!-- SPUR-MANAGED v=1 skill=x sha256=ZZZ -->"
        ).is_none()); // bad hex
    }

    #[test]
    fn parse_marker_accepts_reserved_pointer_id() {
        // `__pointer` uses underscores: reserved for Kiro steering.
        let m = parse_marker(&format!(
            "<!-- SPUR-MANAGED v=1 skill=__pointer sha256={} -->",
            "b".repeat(64)
        ))
        .unwrap();
        assert_eq!(m.skill_id, "__pointer");
    }

    #[test]
    fn sha256_hex_format() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn summary_display_empty_run() {
        let s = Summary::default();
        let rendered = format!("{s}");
        assert!(rendered.contains("wrote 0"));
    }

    #[test]
    fn summary_display_reports_skips() {
        let mut s = Summary::default();
        s.written.push(std::path::PathBuf::from("/x/a"));
        s.skipped.push((std::path::PathBuf::from("/x/b"), SkipReason::UserEdited));
        s.skipped.push((std::path::PathBuf::from("/x/c"), SkipReason::NoMarker));
        let rendered = format!("{s}");
        assert!(rendered.contains("wrote 1"));
        assert!(rendered.contains("skipped 2"));
        assert!(rendered.contains("/x/b"));
    }

    #[test]
    fn atomic_write_creates_missing_dirs_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c/file.md");
        atomic_write(&target, b"hello world").unwrap();
        let read = std::fs::read_to_string(&target).unwrap();
        assert_eq!(read, "hello world");
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.md");
        std::fs::write(&target, "old").unwrap();
        atomic_write(&target, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }
}
