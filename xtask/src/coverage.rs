use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
};

use crate::cargo;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CoverageOptions {
    pub(crate) base: String,
    pub(crate) floor: f64,
    pub(crate) diff_floor: f64,
    pub(crate) output_path: PathBuf,
    pub(crate) dry_run: bool,
}

impl Default for CoverageOptions {
    fn default() -> Self {
        Self {
            base: "main".to_owned(),
            floor: 75.0,
            diff_floor: 85.0,
            output_path: PathBuf::from("coverage/lcov.info"),
            dry_run: false,
        }
    }
}

pub(crate) fn parse_coverage_options(extra: Vec<String>) -> Result<CoverageOptions, String> {
    let mut options = CoverageOptions::default();
    let mut args = extra.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--base" {
            options.base = args
                .next()
                .ok_or_else(|| "--base requires a git ref".to_owned())?;
        } else if let Some(value) = arg.strip_prefix("--base=") {
            options.base = value.to_owned();
        } else if arg == "--floor" {
            let value = args
                .next()
                .ok_or_else(|| "--floor requires a percentage".to_owned())?;
            options.floor = parse_percent("--floor", &value)?;
        } else if let Some(value) = arg.strip_prefix("--floor=") {
            options.floor = parse_percent("--floor", value)?;
        } else if arg == "--diff-floor" {
            let value = args
                .next()
                .ok_or_else(|| "--diff-floor requires a percentage".to_owned())?;
            options.diff_floor = parse_percent("--diff-floor", &value)?;
        } else if let Some(value) = arg.strip_prefix("--diff-floor=") {
            options.diff_floor = parse_percent("--diff-floor", value)?;
        } else if arg == "--output" {
            let value = args
                .next()
                .ok_or_else(|| "--output requires a path".to_owned())?;
            options.output_path = PathBuf::from(value);
        } else if let Some(value) = arg.strip_prefix("--output=") {
            options.output_path = PathBuf::from(value);
        } else if arg == "--dry-run" {
            options.dry_run = true;
        } else {
            return Err(format!("unknown coverage option {arg:?}"));
        }
    }
    Ok(options)
}

/// Per-file, per-line hit counts parsed from an lcov trace file.
#[derive(Debug, Default, Clone)]
pub(crate) struct LineCoverage {
    files: HashMap<String, HashMap<u32, u64>>,
}

impl LineCoverage {
    pub(crate) fn parse_lcov(input: &str) -> Self {
        let mut files: HashMap<String, HashMap<u32, u64>> = HashMap::new();
        let mut current: Option<String> = None;
        for line in input.lines() {
            if let Some(path) = line.strip_prefix("SF:") {
                current = Some(normalize_path(path));
            } else if let Some(rest) = line.strip_prefix("DA:") {
                let Some(path) = current.as_ref() else {
                    continue;
                };
                let mut parts = rest.splitn(3, ',');
                let line_no = parts.next().and_then(|s| s.parse::<u32>().ok());
                let hits = parts.next().and_then(|s| s.parse::<u64>().ok());
                if let (Some(line_no), Some(hits)) = (line_no, hits) {
                    // Multiple end_of_record blocks can restate the same file
                    // (e.g. once per test binary); a line counts as covered
                    // if any block hit it.
                    let entry = files
                        .entry(path.clone())
                        .or_default()
                        .entry(line_no)
                        .or_insert(0);
                    *entry = (*entry).max(hits);
                }
            } else if line == "end_of_record" {
                current = None;
            }
        }
        Self { files }
    }

    pub(crate) fn total_coverage(&self) -> CoverageStats {
        let mut covered = 0u64;
        let mut total = 0u64;
        for lines in self.files.values() {
            for &hits in lines.values() {
                total += 1;
                if hits > 0 {
                    covered += 1;
                }
            }
        }
        CoverageStats { covered, total }
    }

    /// Hit count for `file:line`, or `None` if lcov has no data for that line.
    /// Falls back to a path-suffix match so an absolute `SF:` path (some
    /// cargo-llvm-cov versions/platforms emit these) still matches the
    /// repo-relative paths `git diff` produces.
    pub(crate) fn line_hit(&self, file: &str, line: u32) -> Option<u64> {
        let needle = normalize_path(file);
        if let Some(lines) = self.files.get(&needle) {
            return lines.get(&line).copied();
        }
        let needle_components: Vec<&str> = needle.split('/').collect();
        self.files.iter().find_map(|(path, lines)| {
            let path_components: Vec<&str> = path.split('/').collect();
            path_ends_with(&path_components, &needle_components)
                .then(|| lines.get(&line).copied())
                .flatten()
        })
    }
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_owned()
}

fn path_ends_with(haystack: &[&str], needle: &[&str]) -> bool {
    needle.len() <= haystack.len() && haystack[haystack.len() - needle.len()..] == *needle
}

/// Parses `git diff --unified=0 <base>...HEAD` output into, per changed file,
/// the set of line numbers added or modified on the `HEAD` side. Deletion-only
/// hunks contribute no lines (there's nothing new to demand coverage of).
pub(crate) fn parse_changed_lines(diff_text: &str) -> HashMap<String, HashSet<u32>> {
    let mut result: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut current_file: Option<String> = None;
    let mut next_added_line: u32 = 0;
    for line in diff_text.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = Some(path.to_owned());
            continue;
        }
        if line.starts_with("+++ ") {
            current_file = None;
            continue;
        }
        if let Some(hunk) = line.strip_prefix("@@ ") {
            if let Some((start, _count)) = parse_hunk_new_range(hunk) {
                next_added_line = start;
            }
            continue;
        }
        let Some(file) = current_file.as_ref() else {
            continue;
        };
        if line.starts_with('+') {
            result
                .entry(file.clone())
                .or_default()
                .insert(next_added_line);
            next_added_line += 1;
        }
        // '-' lines (including the "--- a/..." file header) don't consume a
        // line number on the "+" side.
    }
    result
}

/// Parses the `+start[,count]` half of a `@@ -a,b +start,count @@` hunk
/// header. `count` defaults to 1 when git omits it (single-line hunks).
fn parse_hunk_new_range(hunk_after_at: &str) -> Option<(u32, u32)> {
    let plus_part = hunk_after_at
        .split_whitespace()
        .find(|p| p.starts_with('+'))?;
    let spec = plus_part.trim_start_matches('+');
    let mut parts = spec.splitn(2, ',');
    let start: u32 = parts.next()?.parse().ok()?;
    let count: u32 = match parts.next() {
        Some(value) => value.parse().ok()?,
        None => 1,
    };
    Some((start, count))
}

/// Result of comparing measured coverage against the configured floors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GateResult {
    pub(crate) total_pct: f64,
    pub(crate) total_floor: f64,
    pub(crate) diff_pct: Option<f64>,
    pub(crate) diff_floor: f64,
}

impl GateResult {
    pub(crate) fn total_pass(&self) -> bool {
        self.total_pct >= self.total_floor
    }

    pub(crate) fn diff_pass(&self) -> bool {
        self.diff_pct.is_none_or(|pct| pct >= self.diff_floor)
    }

    pub(crate) fn overall_pass(&self) -> bool {
        self.total_pass() && self.diff_pass()
    }

    pub(crate) fn report(&self) -> String {
        let total_line = format!(
            "total coverage: {:.2}% (floor {:.2}%) - {}",
            self.total_pct,
            self.total_floor,
            if self.total_pass() { "PASS" } else { "FAIL" }
        );
        let diff_line = match self.diff_pct {
            Some(pct) => format!(
                "diff coverage:  {:.2}% (floor {:.2}%) - {}",
                pct,
                self.diff_floor,
                if self.diff_pass() { "PASS" } else { "FAIL" }
            ),
            None => "diff coverage:  no coverable changed lines - PASS".to_owned(),
        };
        format!("{total_line}\n{diff_line}")
    }
}

pub(crate) fn evaluate_gate(
    coverage: &LineCoverage,
    changed_lines: &HashMap<String, HashSet<u32>>,
    total_floor: f64,
    diff_floor: f64,
) -> GateResult {
    let total = coverage.total_coverage();
    let mut diff_covered = 0u64;
    let mut diff_total = 0u64;
    for (file, lines) in changed_lines {
        for &line in lines {
            if let Some(hits) = coverage.line_hit(file, line) {
                diff_total += 1;
                if hits > 0 {
                    diff_covered += 1;
                }
            }
        }
    }
    let diff_pct = (diff_total > 0).then(|| diff_covered as f64 / diff_total as f64 * 100.0);
    GateResult {
        total_pct: total.percent(),
        total_floor,
        diff_pct,
        diff_floor,
    }
}

pub(crate) fn git_diff_command(workspace_root: &Path, base: &str) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(workspace_root).args([
        "diff",
        "--unified=0",
        &format!("{base}...HEAD"),
        "--",
        "*.rs",
    ]);
    cmd
}

pub(crate) fn llvm_cov_measure_command(workspace_root: &Path, output_path: &Path) -> Command {
    let mut cmd = Command::new(cargo());
    cmd.current_dir(workspace_root).args([
        "llvm-cov",
        "--workspace",
        "--lib",
        "--lcov",
        "--output-path",
    ]);
    cmd.arg(output_path);
    cmd
}

pub(crate) fn llvm_cov_version_command() -> Command {
    let mut cmd = Command::new(cargo());
    cmd.args(["llvm-cov", "--version"]);
    cmd
}

pub(crate) fn install_llvm_cov_command() -> Command {
    let mut cmd = Command::new(cargo());
    cmd.args(["install", "cargo-llvm-cov", "--locked"]);
    cmd
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoverageStats {
    pub(crate) covered: u64,
    pub(crate) total: u64,
}

impl CoverageStats {
    pub(crate) fn percent(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            self.covered as f64 / self.total as f64 * 100.0
        }
    }
}

fn parse_percent(flag: &str, value: &str) -> Result<f64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} {value:?} is not a number"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn coverage_options_default_matches_spec_thresholds() {
        let options = parse_coverage_options(vec![]).unwrap();

        assert_eq!(options.base, "main");
        assert!((options.floor - 75.0).abs() < 1e-9);
        assert!((options.diff_floor - 85.0).abs() < 1e-9);
        assert_eq!(options.output_path, PathBuf::from("coverage/lcov.info"));
        assert!(!options.dry_run);
    }

    #[test]
    fn coverage_options_parses_overrides() {
        let options = parse_coverage_options(vec![
            "--base".to_owned(),
            "origin/main".to_owned(),
            "--floor=60".to_owned(),
            "--diff-floor".to_owned(),
            "80".to_owned(),
            "--output".to_owned(),
            "out/lcov.info".to_owned(),
            "--dry-run".to_owned(),
        ])
        .unwrap();

        assert_eq!(options.base, "origin/main");
        assert!((options.floor - 60.0).abs() < 1e-9);
        assert!((options.diff_floor - 80.0).abs() < 1e-9);
        assert_eq!(options.output_path, PathBuf::from("out/lcov.info"));
        assert!(options.dry_run);
    }

    #[test]
    fn coverage_options_rejects_unknown_flag() {
        let error = parse_coverage_options(vec!["--bogus".to_owned()]).unwrap_err();

        assert!(error.contains("--bogus"));
    }

    #[test]
    fn coverage_options_rejects_non_numeric_floor() {
        let error =
            parse_coverage_options(vec!["--floor".to_owned(), "abc".to_owned()]).unwrap_err();

        assert!(error.contains("--floor"));
    }

    #[test]
    fn parses_total_coverage_across_multiple_files() {
        let lcov = "\
TN:
SF:crates/foo/src/lib.rs
DA:1,3
DA:2,0
DA:3,1
end_of_record
SF:crates/bar/src/lib.rs
DA:10,0
DA:11,0
end_of_record
";
        let coverage = LineCoverage::parse_lcov(lcov);
        let stats = coverage.total_coverage();

        assert_eq!(stats.covered, 2);
        assert_eq!(stats.total, 5);
        assert!((stats.percent() - 40.0).abs() < 1e-9);
    }

    #[test]
    fn merges_repeated_end_of_record_blocks_for_the_same_file() {
        let lcov = "\
SF:crates/foo/src/lib.rs
DA:1,0
end_of_record
SF:crates/foo/src/lib.rs
DA:1,2
end_of_record
";
        let coverage = LineCoverage::parse_lcov(lcov);
        let stats = coverage.total_coverage();

        assert_eq!(stats.covered, 1);
        assert_eq!(stats.total, 1);
    }

    #[test]
    fn empty_lcov_reports_100_percent_with_zero_lines() {
        let coverage = LineCoverage::parse_lcov("");
        let stats = coverage.total_coverage();

        assert_eq!(stats.total, 0);
        assert!((stats.percent() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn line_hit_returns_hit_count_for_tracked_line() {
        let lcov = "SF:crates/foo/src/lib.rs\nDA:5,2\nend_of_record\n";
        let coverage = LineCoverage::parse_lcov(lcov);

        assert_eq!(coverage.line_hit("crates/foo/src/lib.rs", 5), Some(2));
        assert_eq!(coverage.line_hit("crates/foo/src/lib.rs", 6), None);
    }

    #[test]
    fn line_hit_falls_back_to_suffix_match_for_absolute_sf_paths() {
        let lcov = "SF:/home/build/repo/crates/foo/src/lib.rs\nDA:5,0\nend_of_record\n";
        let coverage = LineCoverage::parse_lcov(lcov);

        assert_eq!(coverage.line_hit("crates/foo/src/lib.rs", 5), Some(0));
    }

    #[test]
    fn line_hit_suffix_match_does_not_match_partial_component() {
        let lcov = "SF:crates/xbar/src/lib.rs\nDA:5,1\nend_of_record\n";
        let coverage = LineCoverage::parse_lcov(lcov);

        assert_eq!(coverage.line_hit("bar/src/lib.rs", 5), None);
    }

    #[test]
    fn parse_hunk_new_range_reads_explicit_count() {
        assert_eq!(
            parse_hunk_new_range("-10,0 +11,3 @@ fn something() {"),
            Some((11, 3))
        );
    }

    #[test]
    fn parse_hunk_new_range_defaults_count_to_one_when_omitted() {
        assert_eq!(parse_hunk_new_range("-1 +1 @@"), Some((1, 1)));
    }

    #[test]
    fn parse_changed_lines_collects_added_lines_and_ignores_deletions() {
        let diff = "\
diff --git a/crates/foo/src/lib.rs b/crates/foo/src/lib.rs
index 1111111..2222222 100644
--- a/crates/foo/src/lib.rs
+++ b/crates/foo/src/lib.rs
@@ -10,0 +11,2 @@ fn existing() {
+    added_line_one();
+    added_line_two();
@@ -20,2 +23,1 @@
-removed_line_a
-removed_line_b
+replacement_line
";
        let changed = parse_changed_lines(diff);

        let lines = changed.get("crates/foo/src/lib.rs").unwrap();
        assert_eq!(lines, &std::collections::HashSet::from([11, 12, 23]));
    }

    #[test]
    fn parse_changed_lines_ignores_deletion_only_hunks() {
        let diff = "\
diff --git a/crates/foo/src/lib.rs b/crates/foo/src/lib.rs
--- a/crates/foo/src/lib.rs
+++ b/crates/foo/src/lib.rs
@@ -5,2 +6,0 @@
-old_line_a
-old_line_b
";
        let changed = parse_changed_lines(diff);

        assert!(changed
            .get("crates/foo/src/lib.rs")
            .is_none_or(|s| s.is_empty()));
    }

    #[test]
    fn parse_changed_lines_handles_multiple_files() {
        let diff = "\
diff --git a/crates/foo/src/lib.rs b/crates/foo/src/lib.rs
--- a/crates/foo/src/lib.rs
+++ b/crates/foo/src/lib.rs
@@ -1,0 +2,1 @@
+foo_new_line
diff --git a/crates/bar/src/lib.rs b/crates/bar/src/lib.rs
--- a/crates/bar/src/lib.rs
+++ b/crates/bar/src/lib.rs
@@ -5,0 +6,1 @@
+bar_new_line
";
        let changed = parse_changed_lines(diff);

        assert_eq!(
            changed.get("crates/foo/src/lib.rs"),
            Some(&std::collections::HashSet::from([2]))
        );
        assert_eq!(
            changed.get("crates/bar/src/lib.rs"),
            Some(&std::collections::HashSet::from([6]))
        );
    }

    #[test]
    fn evaluate_gate_passes_when_both_metrics_meet_floor() {
        let lcov = "\
SF:crates/foo/src/lib.rs
DA:1,1
DA:2,1
DA:3,0
DA:4,1
end_of_record
";
        let coverage = LineCoverage::parse_lcov(lcov);
        let mut changed = std::collections::HashMap::new();
        changed.insert(
            "crates/foo/src/lib.rs".to_owned(),
            std::collections::HashSet::from([1, 2]),
        );

        let result = evaluate_gate(&coverage, &changed, 50.0, 90.0);

        assert!((result.total_pct - 75.0).abs() < 1e-9);
        assert!(result.total_pass());
        assert_eq!(result.diff_pct, Some(100.0));
        assert!(result.diff_pass());
        assert!(result.overall_pass());
    }

    #[test]
    fn evaluate_gate_fails_when_diff_coverage_misses_floor() {
        let lcov = "SF:crates/foo/src/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\n";
        let coverage = LineCoverage::parse_lcov(lcov);
        let mut changed = std::collections::HashMap::new();
        changed.insert(
            "crates/foo/src/lib.rs".to_owned(),
            std::collections::HashSet::from([1, 2]),
        );

        let result = evaluate_gate(&coverage, &changed, 0.0, 90.0);

        assert_eq!(result.diff_pct, Some(50.0));
        assert!(!result.diff_pass());
        assert!(!result.overall_pass());
    }

    #[test]
    fn evaluate_gate_treats_no_coverable_changed_lines_as_diff_pass() {
        let lcov = "SF:crates/foo/src/lib.rs\nDA:1,1\nend_of_record\n";
        let coverage = LineCoverage::parse_lcov(lcov);
        let changed = std::collections::HashMap::new();

        let result = evaluate_gate(&coverage, &changed, 0.0, 90.0);

        assert_eq!(result.diff_pct, None);
        assert!(result.diff_pass());
    }

    #[test]
    fn gate_result_report_lists_pass_and_fail_lines() {
        let result = GateResult {
            total_pct: 40.0,
            total_floor: 75.0,
            diff_pct: Some(100.0),
            diff_floor: 85.0,
        };

        let report = result.report();

        assert!(report.contains("total coverage: 40.00% (floor 75.00%) - FAIL"));
        assert!(report.contains("diff coverage:  100.00% (floor 85.00%) - PASS"));
    }

    #[test]
    fn llvm_cov_measure_command_targets_workspace_lib_lcov() {
        let root = std::path::PathBuf::from("/workspace");
        let output = std::path::PathBuf::from("coverage/lcov.info");

        let command = llvm_cov_measure_command(&root, &output);

        assert_eq!(
            crate::command_args(&command),
            vec![
                "llvm-cov".to_owned(),
                "--workspace".to_owned(),
                "--lib".to_owned(),
                "--lcov".to_owned(),
                "--output-path".to_owned(),
                "coverage/lcov.info".to_owned(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn git_diff_command_uses_three_dot_merge_base_diff() {
        let root = std::path::PathBuf::from("/workspace");

        let command = git_diff_command(&root, "main");

        assert_eq!(
            crate::command_args(&command),
            vec![
                "diff".to_owned(),
                "--unified=0".to_owned(),
                "main...HEAD".to_owned(),
                "--".to_owned(),
                "*.rs".to_owned(),
            ]
        );
        assert_eq!(command.get_program(), std::ffi::OsStr::new("git"));
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn install_llvm_cov_command_uses_locked_install() {
        let command = install_llvm_cov_command();

        assert_eq!(
            crate::command_args(&command),
            vec![
                "install".to_owned(),
                "cargo-llvm-cov".to_owned(),
                "--locked".to_owned(),
            ]
        );
    }

    #[test]
    fn llvm_cov_version_command_checks_version() {
        let command = llvm_cov_version_command();

        assert_eq!(
            crate::command_args(&command),
            vec!["llvm-cov".to_owned(), "--version".to_owned()]
        );
    }
}
