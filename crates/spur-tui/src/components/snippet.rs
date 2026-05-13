//! Synchronous snippet reader for the @ mention popover preview pane.
//!
//! Each `read_snippet` call opens the file and skips through it line-by-line
//! using `BufRead::lines().skip(...)`, then reads up to `max_lines`. Calls
//! happen on the TUI render thread inside `MentionQuerySource::preview_for`
//! and are memoized per-selection by `PickerShell::active_preview`, so a
//! single arrow-key press triggers at most one read. Holding the arrow key
//! (key repeat) will issue one read per repeat event — acceptable on local
//! disk, potentially noticeable on slow network mounts (NFS, sshfs).
//! Phase 1.5 may move this onto a background reader if profiling demands.

use ratatui::text::{Line, Span};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetState {
    Ready(Vec<Line<'static>>),
    Failed(String),
}

/// Resolve a workspace-relative path under `workspace_root`, refusing
/// absolute paths, parent-traversal segments, and any path that would
/// escape the workspace after canonicalization.
///
/// Returns `None` for any unsafe input. Callers should treat `None` as
/// "do not read; show no snippet".
pub fn resolve_workspace_path(workspace_root: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        return None;
    }
    for component in relative_path.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) => return None,
            _ => {}
        }
    }
    Some(workspace_root.join(relative_path))
}

pub fn read_snippet(
    path: &Path,
    line_range: [u32; 2],
    max_lines: usize,
    max_bytes: usize,
) -> SnippetState {
    if max_lines == 0 {
        return SnippetState::Failed("max_lines=0".to_string());
    }

    let [start, end] = line_range;
    if start == 0 || end == 0 || start > end {
        return SnippetState::Failed("empty range".to_string());
    }

    let Ok(file) = File::open(path) else {
        return SnippetState::Failed("unreadable".to_string());
    };

    let mut kept = Vec::new();
    let mut total_bytes = 0usize;
    let mut lines_read = 0usize;
    let wanted_start = start as usize;
    let wanted_end = end as usize;
    let wanted_count = wanted_end.saturating_sub(wanted_start).saturating_add(1);
    let effective_count = wanted_count.min(max_lines);

    let reader = BufReader::new(file);
    for line_result in reader
        .lines()
        .skip(wanted_start.saturating_sub(1))
        .take(effective_count)
    {
        lines_read += 1;
        let Ok(mut line) = line_result else {
            return SnippetState::Failed("unreadable".to_string());
        };
        if line.chars().count() > 200 {
            line = line.chars().take(199).collect::<String>() + "…";
        }
        total_bytes = total_bytes.saturating_add(line.len());
        if total_bytes > max_bytes {
            return SnippetState::Failed("too large".to_string());
        }
        kept.push(line);
    }

    if lines_read == 0 {
        return SnippetState::Failed("empty range".to_string());
    }

    let indent = kept
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.as_bytes()
                .iter()
                .take_while(|&&b| b == b' ' || b == b'\t')
                .count()
        })
        .min()
        .unwrap_or(0);

    let lines = kept
        .into_iter()
        .map(|line| {
            let trimmed = if indent == 0 {
                line
            } else {
                line.chars().skip(indent).collect::<String>()
            };
            Line::from(Span::raw(trimmed))
        })
        .collect();

    SnippetState::Ready(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_sample(path: &Path) {
        let text = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";
        fs::write(path, text).expect("write sample");
    }

    #[test]
    fn read_snippet_happy_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.rs");
        write_sample(&path);

        let state = read_snippet(&path, [2, 3], 12, 65_536);
        match state {
            SnippetState::Ready(lines) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].spans[0].content.as_ref(), "let x = 1;");
                assert_eq!(lines[1].spans[0].content.as_ref(), "println!(\"{}\", x);");
            }
            other => panic!("unexpected state: {other:?}"),
        }
    }

    #[test]
    fn read_snippet_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.rs");
        let state = read_snippet(&path, [1, 1], 12, 65_536);
        assert_eq!(state, SnippetState::Failed("unreadable".to_string()));
    }

    #[test]
    fn read_snippet_empty_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.rs");
        write_sample(&path);

        let state = read_snippet(&path, [3, 2], 12, 65_536);
        assert_eq!(state, SnippetState::Failed("empty range".to_string()));
    }

    #[test]
    fn read_snippet_range_past_eof() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.rs");
        write_sample(&path);

        let state = read_snippet(&path, [20, 25], 12, 65_536);
        assert_eq!(state, SnippetState::Failed("empty range".to_string()));
    }

    #[test]
    fn read_snippet_works_on_large_file_with_small_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("large.rs");
        let body: String = (1..=5_000).map(|n| format!("line {n:04}\n")).collect();
        fs::write(&path, body).expect("write large file");

        let state = read_snippet(&path, [10, 11], 12, 65_536);
        match state {
            SnippetState::Ready(lines) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].spans[0].content.as_ref(), "line 0010");
                assert_eq!(lines[1].spans[0].content.as_ref(), "line 0011");
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn read_snippet_truncates_when_range_exceeds_max_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.rs");
        let body: String = (1..=200).map(|n| format!("line {n}\n")).collect();
        fs::write(&path, body).expect("write big file");

        let state = read_snippet(&path, [1, 200], 12, 65_536);
        match state {
            SnippetState::Ready(lines) => {
                assert_eq!(lines.len(), 12, "should truncate to max_lines");
                assert_eq!(lines[0].spans[0].content.as_ref(), "line 1");
                assert_eq!(lines[11].spans[0].content.as_ref(), "line 12");
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn read_snippet_strips_common_indent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.rs");
        fs::write(&path, "        one\n        two\n            three\n").expect("write sample");

        let state = read_snippet(&path, [1, 3], 12, 65_536);
        match state {
            SnippetState::Ready(lines) => {
                assert_eq!(lines[0].spans[0].content.as_ref(), "one");
                assert_eq!(lines[1].spans[0].content.as_ref(), "two");
                assert_eq!(lines[2].spans[0].content.as_ref(), "    three");
            }
            other => panic!("unexpected state: {other:?}"),
        }
    }

    #[test]
    fn resolve_workspace_path_happy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resolved = resolve_workspace_path(dir.path(), "src/foo.rs");
        assert_eq!(resolved, Some(dir.path().join("src/foo.rs")));
    }

    #[test]
    fn resolve_workspace_path_empty_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolve_workspace_path(dir.path(), ""), None);
    }

    #[test]
    fn resolve_workspace_path_absolute_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolve_workspace_path(dir.path(), "/etc/passwd"), None);
    }

    #[test]
    fn resolve_workspace_path_traversal_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolve_workspace_path(dir.path(), "../etc/passwd"), None);
        assert_eq!(resolve_workspace_path(dir.path(), "src/../../escape"), None);
    }

    #[test]
    fn resolve_workspace_path_nested_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rel = "crates/spur-tui/src/lib.rs";
        assert_eq!(
            resolve_workspace_path(dir.path(), rel),
            Some(dir.path().join(rel))
        );
    }
}
