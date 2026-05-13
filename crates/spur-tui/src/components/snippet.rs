use ratatui::text::{Line, Span};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnippetState {
    Ready(Vec<Line<'static>>),
    Failed(String),
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
    let Ok(meta) = file.metadata() else {
        return SnippetState::Failed("unreadable".to_string());
    };
    if meta.len() as usize > max_bytes {
        return SnippetState::Failed("too large".to_string());
    }

    let mut kept = Vec::new();
    let mut total_bytes = 0usize;
    let mut lines_read = 0usize;
    let wanted_start = start as usize;
    let wanted_end = end as usize;
    let wanted_count = wanted_end.saturating_sub(wanted_start).saturating_add(1);

    let reader = BufReader::new(file);
    for line_result in reader
        .lines()
        .skip(wanted_start.saturating_sub(1))
        .take(wanted_count)
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
        if kept.len() > max_lines {
            return SnippetState::Failed("too many lines".to_string());
        }
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
    fn read_snippet_oversize_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.rs");
        fs::write(&path, "x".repeat(70_000)).expect("write sample");

        let state = read_snippet(&path, [1, 1], 12, 65_536);
        assert_eq!(state, SnippetState::Failed("too large".to_string()));
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
}
