//! Shared fixtures for the sud-m2a engine-core contract tests
//! (`spur-mentions`, 2026-08-27 mentions spec §§2-5).

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use spur_mentions::{
    MentionEntry, MentionId, MentionKind, MentionSource, SourceBuildError, SourceContext,
    SourceSnapshot,
};

/// Std-only temporary tree: a unique directory under `std::env::temp_dir()`,
/// removed recursively on drop. No dev-dependency on `tempfile` — the
/// dependency topology gates forbid growing the crate's dep set casually.
pub struct TempTree {
    root: PathBuf,
}

impl TempTree {
    pub fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "spur-mentions-{tag}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("temp tree root creation");
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create a directory (with parents) and return its path.
    pub fn dir(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        fs::create_dir_all(&path).expect("temp tree dir creation");
        path
    }

    /// Write a file (with parent dirs) and return its path.
    pub fn file(&self, rel: &str, contents: &str) -> PathBuf {
        if let Some(parent) = Path::new(rel).parent() {
            let parent = self.root.join(parent);
            fs::create_dir_all(parent).expect("temp tree file parents");
        }
        let path = self.root.join(rel);
        fs::write(&path, contents).expect("temp tree file write");
        path
    }

    /// Create an empty `.git` directory so the `ignore` walker treats the
    /// tree as a repository root and applies `.gitignore` rules
    /// (`require_git` defaults to true in the walker).
    pub fn git_dir(&self) {
        self.dir(".git");
    }

    /// Create a directory symlink `link_rel -> target_rel` (unix only).
    #[cfg(unix)]
    pub fn symlink_dir(&self, target_rel: &str, link_rel: &str) {
        let target = self.root.join(target_rel);
        let link = self.root.join(link_rel);
        std::os::unix::fs::symlink(&target, &link).expect("temp tree symlink");
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Out-of-band control and observation for a [`ScriptedSource`] that stays
/// valid after the source is boxed and handed to the engine.
#[derive(Clone, Default)]
pub struct SourceHandle {
    /// Number of `build` calls, including failed ones.
    pub builds: Arc<AtomicUsize>,
    /// When set, the next `build` fails.
    pub fail: Arc<AtomicBool>,
    /// When set (with `fail`), the failure is traversal-typed.
    pub traversal: Arc<AtomicBool>,
    /// Value returned by `source_token`.
    pub token: Arc<AtomicU64>,
}

/// Mention source with scriptable failure/token, batched entries, and an
/// observable build count. Build #n returns batch `n-1`; batches beyond the
/// last supplied one fall back to the final batch.
pub struct ScriptedSource {
    key: &'static str,
    handle: SourceHandle,
    batches: Vec<Vec<MentionEntry>>,
}

impl ScriptedSource {
    pub fn new(key: &'static str, batches: Vec<Vec<MentionEntry>>) -> (Self, SourceHandle) {
        let handle = SourceHandle::default();
        (
            Self {
                key,
                handle: handle.clone(),
                batches,
            },
            handle,
        )
    }
}

impl MentionSource for ScriptedSource {
    fn key(&self) -> &str {
        self.key
    }

    fn source_token(&self) -> u64 {
        self.handle.token.load(Ordering::Relaxed)
    }

    fn build(
        &mut self,
        _root: &Path,
        _context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError> {
        let build_number = self.handle.builds.fetch_add(1, Ordering::Relaxed) + 1;
        if self.handle.fail.load(Ordering::Relaxed) {
            return if self.handle.traversal.load(Ordering::Relaxed) {
                Err(SourceBuildError::traversal("scripted traversal failure"))
            } else {
                Err(SourceBuildError::new("scripted build failure"))
            };
        }
        let index = build_number.clamp(1, self.batches.len().max(1)) - 1;
        let entries = self.batches.get(index).cloned().unwrap_or_default();
        Ok(SourceSnapshot::new(entries, 0))
    }
}

/// Neutral entry fixture.
pub fn entry(id: u64, kind: MentionKind, uri: &str, display: &str) -> MentionEntry {
    MentionEntry {
        id: MentionId::new(id),
        kind,
        uri: uri.to_owned(),
        display: display.to_owned(),
        secondary: None,
        search_text: None,
        insert_text: None,
    }
}

/// `file://` URI exactly the way today's TUI `entry_for_path` builds it:
/// `format!("file://{abs}")` with no percent-encoding.
pub fn legacy_file_uri(abs: &Path) -> String {
    format!("file://{}", abs.display())
}
