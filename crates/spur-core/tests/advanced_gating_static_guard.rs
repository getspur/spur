//! Guards against accidental ungated `.advanced()` additions in the relocated
//! MCP engine/server surface.
//! If this fails, the new callsite needs a `require_feature` gate above it.

use std::fs;
use std::path::{Path, PathBuf};

const REQUIRED_FUNCTION: &str = "require_feature(";
const REQUIRED_KEY: &str = "PM_PRO_BEADS_ADVANCED";
const LOOKBACK_LINES: usize = 10;
const MIN_GATED_CALLSITES: usize = 29;
const MOVED_SOURCE_PATHS: &[&str] = &[
    "src/handlers.rs",
    "src/outcome_materializer.rs",
    "src/plan",
    "src/server",
    "src/submit_plan_dedup.rs",
    "src/tool_schemas.rs",
    "src/worker_server.rs",
];

#[test]
fn every_advanced_callsite_is_pm_pro_gated() {
    let mut files = Vec::new();
    for source_path in MOVED_SOURCE_PATHS {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(source_path);
        if path.is_dir() {
            collect_rust_files(&path, &mut files);
        } else {
            files.push(path);
        }
    }

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for file in files {
        let content = fs::read_to_string(&file).expect("read source file");
        let lines: Vec<&str> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if !line.contains(".advanced()") || line.trim_start().starts_with("//") {
                continue;
            }
            checked += 1;
            if has_nearby_gate(&lines, idx) || containing_function_has_gate(&lines, idx) {
                continue;
            }
            failures.push(format!("{}:{}", file.display(), idx + 1));
        }
    }

    assert!(
        failures.is_empty(),
        "ungated .advanced() callsites:\n{}",
        failures.join("\n")
    );
    assert!(
        checked >= MIN_GATED_CALLSITES,
        "expected to guard at least {MIN_GATED_CALLSITES} .advanced() callsites, saw {checked}"
    );
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn has_nearby_gate(lines: &[&str], idx: usize) -> bool {
    let start = idx.saturating_sub(LOOKBACK_LINES);
    let window = lines[start..idx].join("\n");
    window.contains(REQUIRED_FUNCTION) && window.contains(REQUIRED_KEY)
}

fn containing_function_has_gate(lines: &[&str], idx: usize) -> bool {
    let Some(fn_start) = (0..=idx)
        .rev()
        .find(|line_idx| lines[*line_idx].contains("fn "))
    else {
        return false;
    };
    let body_to_call = lines[fn_start..=idx].join("\n");
    body_to_call.contains(REQUIRED_FUNCTION) && body_to_call.contains(REQUIRED_KEY)
}
