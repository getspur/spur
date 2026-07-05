use std::path::{Path, PathBuf};

#[test]
fn pack_modules_stay_small_and_helper_oriented() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pack_dir = manifest_dir.join("src").join("pack");
    let modules = [
        "caveats.rs",
        "evidence.rs",
        "graph_reasoning.rs",
        "impact.rs",
        "mod.rs",
        "next_tools.rs",
        "request.rs",
        "response.rs",
        "service.rs",
        "staleness.rs",
    ];

    for module in modules {
        let path = pack_dir.join(module);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let metrics = SourceMetrics::measure(&source);

        assert!(
            metrics.file_loc < 500,
            "{} has {} lines; keep pack files under 500 LOC",
            rel(&manifest_dir, &path).display(),
            metrics.file_loc
        );
        assert!(
            metrics.symbols < 500,
            "{} declares {} symbols; keep module symbol counts under 500",
            rel(&manifest_dir, &path).display(),
            metrics.symbols
        );
        assert!(
            metrics.max_function_loc <= 80,
            "{} has a {}-line function; split long pack helpers",
            rel(&manifest_dir, &path).display(),
            metrics.max_function_loc
        );
    }
}

#[derive(Debug, Default)]
struct SourceMetrics {
    file_loc: usize,
    symbols: usize,
    max_function_loc: usize,
}

impl SourceMetrics {
    fn measure(source: &str) -> Self {
        let lines = source.lines().collect::<Vec<_>>();
        let mut metrics = Self {
            file_loc: lines.len(),
            symbols: lines.iter().filter(|line| is_symbol_line(line)).count(),
            max_function_loc: 0,
        };

        let mut index = 0;
        while index < lines.len() {
            if is_function_line(lines[index]) {
                let end = function_end(&lines, index);
                metrics.max_function_loc = metrics.max_function_loc.max(end - index + 1);
                index = end + 1;
            } else {
                index += 1;
            }
        }

        metrics
    }
}

fn function_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0isize;
    let mut saw_body = false;

    for (offset, line) in lines[start..].iter().enumerate() {
        for ch in line.chars() {
            match ch {
                '{' => {
                    saw_body = true;
                    depth += 1;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if saw_body && depth <= 0 {
            return start + offset;
        }
    }

    lines.len().saturating_sub(1)
}

fn is_function_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("fn ")
        || trimmed.starts_with("async fn ")
        || trimmed.starts_with("pub fn ")
        || trimmed.starts_with("pub async fn ")
        || trimmed.starts_with("pub(crate) fn ")
        || trimmed.starts_with("pub(crate) async fn ")
}

fn is_symbol_line(line: &&str) -> bool {
    let trimmed = line.trim_start();
    is_function_line(trimmed)
        || trimmed.starts_with("struct ")
        || trimmed.starts_with("pub(crate) struct ")
        || trimmed.starts_with("enum ")
        || trimmed.starts_with("pub(crate) enum ")
        || trimmed.starts_with("trait ")
        || trimmed.starts_with("impl ")
}

fn rel<'a>(base: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(base).unwrap_or(path)
}
