//! Skills installer: renders bundled+override skills into per-adapter
//! agent dirs, protects user hand-edits via an in-file marker + sha256.

use crate::skills::adapters::{render_kiro_steering_pointer, Adapter};
use crate::skills::adapters::RenderedFile;
use crate::skills::list_active_skills;
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

/// Outcome of `decide()` for a single target file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Create,
    Update,
    NoOp,
    Skip(SkipReason),
}

/// Return the bytes after the SPUR-MANAGED marker line, if present.
/// Searches the first few lines (tolerates optional YAML frontmatter).
fn body_after_marker(bytes: &[u8]) -> Option<(Marker, &[u8])> {
    let text = std::str::from_utf8(bytes).ok()?;
    for (line, rest_start) in iter_lines_with_positions(text) {
        if let Some(m) = parse_marker(line) {
            return Some((m, &bytes[rest_start..]));
        }
        // Optimization: bail after ~20 lines; marker should be near the top.
        if rest_start > 2048 {
            break;
        }
    }
    None
}

fn iter_lines_with_positions(text: &str) -> impl Iterator<Item = (&str, usize)> {
    text.split_inclusive('\n').scan(0usize, |pos, line| {
        *pos += line.len();
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        Some((trimmed, *pos))
    })
}

fn rendered_without_marker(rf: &RenderedFile) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(&rf.bytes).ok()?;
    let mut out = Vec::with_capacity(rf.bytes.len());
    let mut line_start = 0usize;

    for (line, rest_start) in iter_lines_with_positions(text) {
        if parse_marker(line).is_some() {
            out.extend_from_slice(&rf.bytes[..line_start]);
            out.extend_from_slice(&rf.bytes[rest_start..]);
            return Some(out);
        }
        line_start = rest_start;
    }

    None
}

fn is_unmanaged_spur_override(rf: &RenderedFile, disk: &[u8]) -> bool {
    let components: Vec<_> = rf.path.iter().collect();
    if !components
        .windows(2)
        .any(|window| window[0] == ".spur" && window[1] == "skills")
    {
        return false;
    }

    rendered_without_marker(rf).is_some_and(|bytes| bytes == disk)
}

pub(crate) fn decide(rf: &RenderedFile) -> Result<Decision, InstallError> {
    if !rf.path.exists() {
        return Ok(Decision::Create);
    }
    let disk = std::fs::read(&rf.path).map_err(|source| InstallError::Io {
        path: rf.path.clone(),
        source,
    })?;
    if disk == rf.bytes {
        return Ok(Decision::NoOp);
    }
    let Some((marker, body)) = body_after_marker(&disk) else {
        if is_unmanaged_spur_override(rf, &disk) {
            return Ok(Decision::Update);
        }
        return Ok(Decision::Skip(SkipReason::NoMarker));
    };
    let disk_hash = sha256_hex(body);
    if disk_hash == marker.sha256 {
        Ok(Decision::Update)
    } else {
        Ok(Decision::Skip(SkipReason::UserEdited))
    }
}

/// Render the active skill set into every known agent directory under
/// `repo_root`. Returns a structured Summary of what was written, what
/// was unchanged, and what was skipped.
pub fn run(repo_root: &Path) -> Result<Summary, InstallError> {
    let skills = list_active_skills(repo_root)?;
    let mut summary = Summary::default();

    // Per-skill × per-adapter fanout.
    for skill in &skills {
        for adapter in Adapter::all() {
            let rf = adapter.render(skill, repo_root);
            apply(&rf, &mut summary)?;
        }
    }

    // Once-per-run files (currently only Kiro steering pointer).
    apply(&render_kiro_steering_pointer(repo_root), &mut summary)?;

    Ok(summary)
}

fn apply(rf: &RenderedFile, summary: &mut Summary) -> Result<(), InstallError> {
    match decide(rf)? {
        Decision::Create | Decision::Update => {
            atomic_write(&rf.path, &rf.bytes)?;
            summary.written.push(rf.path.clone());
        }
        Decision::NoOp => {
            summary.unchanged.push(rf.path.clone());
        }
        Decision::Skip(reason) => {
            summary.skipped.push((rf.path.clone(), reason));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::adapters::RenderedFile;

    fn rf_with(path: std::path::PathBuf, bytes: Vec<u8>) -> RenderedFile {
        RenderedFile { path, bytes }
    }

    fn wrap_with_marker(body: &str, skill_id: &str) -> Vec<u8> {
        let marker = Marker {
            version: 1,
            skill_id: skill_id.to_string(),
            sha256: sha256_hex(body.as_bytes()),
        };
        format!("---\nfoo: bar\n---\n{m}{body}", m = marker.render())
            .into_bytes()
    }

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

    #[test]
    fn decide_create_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let rf = rf_with(dir.path().join("x.md"), b"body".to_vec());
        assert_eq!(decide(&rf).unwrap(), Decision::Create);
    }

    #[test]
    fn decide_noop_when_bytes_identical() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        let bytes = wrap_with_marker("hello", "tdd");
        std::fs::write(&target, &bytes).unwrap();
        let rf = rf_with(target, bytes);
        assert_eq!(decide(&rf).unwrap(), Decision::NoOp);
    }

    #[test]
    fn decide_update_when_marker_body_hash_matches_but_bytes_differ() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        // Disk: old-version frontmatter + same body.
        let marker = Marker {
            version: 1,
            skill_id: "tdd".to_string(),
            sha256: sha256_hex(b"hello"),
        };
        let on_disk = format!("---\nold: fm\n---\n{m}hello", m = marker.render());
        std::fs::write(&target, &on_disk).unwrap();
        // Rendered: new frontmatter + same body + same hash.
        let rendered = format!("---\nnew: fm\n---\n{m}hello", m = marker.render());
        let rf = rf_with(target, rendered.into_bytes());
        assert_eq!(decide(&rf).unwrap(), Decision::Update);
    }

    #[test]
    fn decide_skip_usermarker_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        std::fs::write(&target, "totally user's file").unwrap();
        let rf = rf_with(target, b"spur version".to_vec());
        assert_eq!(
            decide(&rf).unwrap(),
            Decision::Skip(SkipReason::NoMarker),
        );
    }

    #[test]
    fn decide_skip_user_edited_when_body_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        // Disk: marker claims body hash of "hello", but body is "edited".
        let marker = Marker {
            version: 1,
            skill_id: "tdd".to_string(),
            sha256: sha256_hex(b"hello"),
        };
        let on_disk = format!("---\nfm: x\n---\n{m}edited body", m = marker.render());
        std::fs::write(&target, &on_disk).unwrap();
        let rf = rf_with(target, b"anything".to_vec());
        assert_eq!(
            decide(&rf).unwrap(),
            Decision::Skip(SkipReason::UserEdited),
        );
    }

    #[test]
    fn run_creates_all_seven_adapter_files_for_one_skill() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join(".spur/skills/my-skill");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\nMy body\n",
        )
        .unwrap();
        // Clear bundled set by not relying on it — bundled skills will also
        // be written, but we only assert our override landed.
        let summary = run(dir.path()).unwrap();
        // Expected paths for `my-skill` (override) across 7 adapters + Kiro pointer.
        for expected in [
            ".spur/skills/my-skill/SKILL.md",
            ".claude/skills/spurpower-my-skill/SKILL.md",
            ".codex/skills/spurpower-my-skill/SKILL.md",
            ".gemini/skills/spurpower-my-skill/SKILL.md",
            ".kiro/skills/spurpower-my-skill/SKILL.md",
            ".opencode/skills/spurpower-my-skill/SKILL.md",
            ".cursor/rules/spurpower-my-skill.mdc",
            ".kiro/steering/spurpower-pointer.md",
        ] {
            let p = dir.path().join(expected);
            assert!(p.exists(), "missing {expected}");
            assert!(summary.written.contains(&p), "not in summary.written: {expected}");
        }
    }
}
