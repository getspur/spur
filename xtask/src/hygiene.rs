//! Continuous hygiene gate for the spur-utilities crate family (spec
//! docs/superpowers/specs/2026-09-23-spur-utilities-tui-decoupling-design.md,
//! §5 "Continuous hygiene gate" + §0 Derived table).
//!
//! Three dependency-tree rules over `cargo tree` text, each failing with a
//! message naming the offending crate:
//!
//! a. `cargo tree -p spur-mentions -e normal` (default features) contains
//!    neither `spur-acp` nor `ratatui`. ACP neutrality of spur-mentions is a
//!    **default-features** property: with `code`, the tree reaches
//!    `spur-acp` transitively via `spur-graph -> spur-mcp` (proved:
//!    `sol_831d5e7c3a7b4fcc`), so this check must never run with
//!    `--features code` or `--all-features`.
//! b. `cargo tree -p spur-mentions --features code -e normal` contains
//!    neither `ratatui` nor `spur-tui`. `spur-acp` IS expected here and is
//!    deliberately not forbidden.
//! c. `cargo tree -p spur-utilities --depth 1 -e normal` lists exactly
//!    `spur-commands` and `spur-mentions` — the facade holds re-exports
//!    only. Its FULL tree legitimately contains `spur-acp` through
//!    spur-commands, hence the `--depth 1` scope (never the full tree).
//!
//! The family crates land in migration phases (spec §5), so checks for
//! crates that are not workspace members yet are skipped with a note —
//! mirroring the incremental topology solve — and become enforcing the
//! moment the crate is scaffolded.

use std::{path::Path, process::Command};

use crate::{cargo, run_output};

const MENTIONS_PACKAGE: &str = "spur-mentions";
const FACADE_PACKAGE: &str = "spur-utilities";

/// Forbidden crates in the default-features `spur-mentions` tree.
const MENTIONS_DEFAULT_FORBIDDEN: [&str; 2] = ["spur-acp", "ratatui"];
/// Forbidden crates in the `--features code` `spur-mentions` tree. `spur-acp`
/// is deliberately absent: with `code` it is reached transitively via
/// `spur-graph -> spur-mcp` and no ACP type crosses the API (spec §2.2).
const MENTIONS_CODE_FORBIDDEN: [&str; 2] = ["ratatui", "spur-tui"];
/// The facade's only legal direct dependencies (spec §2.3).
const FACADE_DIRECT_DEPS: [&str; 2] = ["spur-commands", "spur-mentions"];

pub(crate) fn run_hygiene(workspace_root: &Path) -> Result<(), String> {
    let mut metadata_cmd = cargo_metadata_command(workspace_root);
    let metadata = run_output(&mut metadata_cmd, "cargo metadata --no-deps")?;

    if workspace_has_package(&metadata, MENTIONS_PACKAGE) {
        let mut default_tree_cmd = cargo_tree_command(workspace_root, &["-p", MENTIONS_PACKAGE]);
        let default_tree = run_output(
            &mut default_tree_cmd,
            "cargo tree -p spur-mentions -e normal",
        )?;
        check_forbidden_absent(
            "spur-mentions (default features)",
            &default_tree,
            &MENTIONS_DEFAULT_FORBIDDEN,
        )?;

        let mut code_tree_cmd = cargo_tree_command(
            workspace_root,
            &["-p", MENTIONS_PACKAGE, "--features", "code"],
        );
        let code_tree = run_output(
            &mut code_tree_cmd,
            "cargo tree -p spur-mentions --features code -e normal",
        )?;
        check_forbidden_absent(
            "spur-mentions (--features code)",
            &code_tree,
            &MENTIONS_CODE_FORBIDDEN,
        )?;
    } else {
        eprintln!(
            "==> hygiene: {MENTIONS_PACKAGE} is not a workspace member yet; skipping its tree checks"
        );
    }

    if workspace_has_package(&metadata, FACADE_PACKAGE) {
        let mut facade_tree_cmd =
            cargo_tree_command(workspace_root, &["-p", FACADE_PACKAGE, "--depth", "1"]);
        let facade_tree = run_output(
            &mut facade_tree_cmd,
            "cargo tree -p spur-utilities --depth 1 -e normal",
        )?;
        check_direct_deps_exact(FACADE_PACKAGE, &facade_tree, &FACADE_DIRECT_DEPS)?;
    } else {
        eprintln!(
            "==> hygiene: {FACADE_PACKAGE} is not a workspace member yet; skipping the facade direct-deps check"
        );
    }

    Ok(())
}

// ---- pure `cargo tree` text parsing -----------------------------------------

/// One nesting unit of a `cargo tree` line prefix: a node marker
/// (`├── ` / `└── `) or a continuation marker (`│   ` / four spaces).
const TREE_PREFIX_UNITS: [&str; 4] = ["├── ", "└── ", "│   ", "    "];

/// Split one `cargo tree` line into `(depth, payload)`, where depth counts
/// nesting units (0 = the queried root package) and payload starts at the
/// crate name. Returns `None` for blank lines.
fn split_tree_line(line: &str) -> Option<(usize, &str)> {
    let mut depth = 0usize;
    let mut rest = line;
    while let Some(tail) = TREE_PREFIX_UNITS
        .iter()
        .find_map(|unit| rest.strip_prefix(unit))
    {
        rest = tail;
        depth += 1;
    }
    let payload = rest.trim_start();
    if payload.is_empty() {
        None
    } else {
        Some((depth, payload))
    }
}

/// The crate name from a tree payload like `spur-acp v1.24.0 (/path)` or
/// `serde v1.0.228 (*)` — the first whitespace-separated token.
fn crate_name(payload: &str) -> &str {
    payload.split_whitespace().next().unwrap_or("")
}

/// Forbidden crate names that appear anywhere in the tree (at any depth), in
/// the order they were requested.
pub(crate) fn find_forbidden(tree: &str, forbidden: &[&str]) -> Vec<String> {
    let present: std::collections::HashSet<&str> = tree
        .lines()
        .filter_map(split_tree_line)
        .map(|(_, payload)| crate_name(payload))
        .collect();
    forbidden
        .iter()
        .filter(|name| present.contains(**name))
        .map(|name| (*name).to_owned())
        .collect()
}

/// Direct (depth-1) dependency names of the queried package, in tree order,
/// deduplicated.
pub(crate) fn direct_deps(tree: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (depth, payload) in tree.lines().filter_map(split_tree_line) {
        if depth == 1 {
            let name = crate_name(payload).to_owned();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

// ---- pure workspace-membership scan -----------------------------------------

/// Whether `cargo metadata --no-deps --format-version 1` output lists
/// `package` as a workspace member.
///
/// Matches the exact compact-JSON field `"name":"<package>"`. Other members'
/// dependency arrays can repeat a `"name"` key, but that cannot cause a
/// false *skip*: the two names queried here are internal path crates (never
/// registry deps), and a member referencing a missing path dependency makes
/// `cargo metadata` itself fail loudly before this scan runs.
pub(crate) fn workspace_has_package(metadata_json: &str, package: &str) -> bool {
    metadata_json.contains(&format!("\"name\":\"{package}\""))
}

// ---- rule evaluation (pure; over captured tree text) -------------------------

/// Rule shape (a)/(b): none of `forbidden` may appear anywhere in the tree.
/// The error names every offending crate.
pub(crate) fn check_forbidden_absent(
    scope: &str,
    tree: &str,
    forbidden: &[&str],
) -> Result<(), String> {
    let offenders = find_forbidden(tree, forbidden);
    if offenders.is_empty() {
        return Ok(());
    }
    Err(format!(
        "hygiene: {scope} dependency tree contains forbidden crate(s): {}",
        offenders.join(", ")
    ))
}

/// Rule shape (c): direct dependencies are exactly `expected`. The error
/// names every missing and every unexpected crate.
pub(crate) fn check_direct_deps_exact(
    package: &str,
    tree: &str,
    expected: &[&str],
) -> Result<(), String> {
    let actual = direct_deps(tree);
    let missing: Vec<String> = expected
        .iter()
        .filter(|name| !actual.iter().any(|dep| dep.as_str() == **name))
        .map(|name| (*name).to_owned())
        .collect();
    let unexpected: Vec<String> = actual
        .iter()
        .filter(|dep| !expected.contains(&dep.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() && unexpected.is_empty() {
        return Ok(());
    }
    let mut detail = Vec::new();
    if !missing.is_empty() {
        detail.push(format!("missing: {}", missing.join(", ")));
    }
    if !unexpected.is_empty() {
        detail.push(format!("unexpected: {}", unexpected.join(", ")));
    }
    Err(format!(
        "hygiene: {package} direct dependencies must be exactly [{}]; {}",
        expected.join(", "),
        detail.join("; ")
    ))
}

// ---- cargo invocation --------------------------------------------------------

fn cargo_metadata_command(workspace_root: &Path) -> Command {
    let mut cmd = Command::new(cargo());
    cmd.args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root);
    cmd
}

fn cargo_tree_command(workspace_root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(cargo());
    cmd.arg("tree")
        .args(args)
        .args(["-e", "normal"])
        .current_dir(workspace_root);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_args;
    use std::path::PathBuf;

    /// Expected shape of `cargo tree -p spur-mentions -e normal` once the
    /// extraction lands: neutral engine deps only — no `spur-acp`, no
    /// `ratatui`.
    const MENTIONS_DEFAULT_TREE: &str = "\
spur-mentions v1.24.0 (/ws/crates/spur-utilities/mentions)
├── ignore v1.11.1
│   ├── crossbeam-channel v0.5.15
│   └── walkdir v2.5.0
├── nucleo-matcher v0.3.5
│   ├── memchr v2.7.4
│   └── regex-automata v0.4.9
└── url v2.5.7
    ├── form_urlencoded v1.2.2
    ├── idna v1.1.0
    │   └── icu_normalizer v1.5.0
    └── percent-encoding v2.3.2
";

    /// The default-features rule failing twice over: a TUI crate at depth 1
    /// and a direct-ish ACP edge smuggled in deeper (nested under a last
    /// child, so the continuation prefix is four spaces, not `│`).
    const MENTIONS_DEFAULT_TREE_VIOLATING: &str = "\
spur-mentions v1.24.0 (/ws/crates/spur-utilities/mentions)
├── ignore v1.11.1
│   └── walkdir v2.5.0
├── nucleo-matcher v0.3.5
│   ├── memchr v2.7.4
│   └── regex-automata v0.4.9
├── ratatui v0.29.0
│   ├── bitflags v2.11.0
│   └── serde v1.0.228
├── serde v1.0.228 (*)
└── spur-session-adapter v1.24.0 (/ws/crates/spur-session-adapter)
    └── spur-acp v1.24.0 (/ws/crates/spur-acp)
        └── agent-client-protocol v0.11.0
";

    /// Expected shape of `cargo tree -p spur-mentions --features code -e
    /// normal`: `spur-acp` appears transitively via
    /// `spur-graph -> spur-mcp` and is therefore NOT forbidden here, while
    /// `ratatui`/`spur-tui` still are.
    const MENTIONS_CODE_TREE: &str = "\
spur-mentions v1.24.0 (/ws/crates/spur-utilities/mentions)
├── ignore v1.11.1
├── nucleo-matcher v0.3.5
├── spur-graph v1.24.0 (/ws/crates/spur-graph)
│   ├── petgraph v0.8.1
│   └── spur-mcp v1.24.0 (/ws/crates/spur-mcp)
│       ├── anyhow v1.0.102
│       └── spur-acp v1.24.0 (/ws/crates/spur-acp)
│           └── agent-client-protocol v0.11.0
└── url v2.5.7
";

    /// Expected shape of `cargo tree -p spur-utilities --depth 1 -e normal`:
    /// re-exports only, exactly the two family crates.
    const FACADE_TREE_CLEAN: &str = "\
spur-utilities v1.24.0 (/ws/crates/spur-utilities/facade)
├── spur-commands v1.24.0 (/ws/crates/spur-utilities/commands)
└── spur-mentions v1.24.0 (/ws/crates/spur-utilities/mentions)
";

    /// The facade rule failing via an extra direct dependency.
    const FACADE_TREE_EXTRA_DEP: &str = "\
spur-utilities v1.24.0 (/ws/crates/spur-utilities/facade)
├── serde_json v1.0.149
├── spur-commands v1.24.0 (/ws/crates/spur-utilities/commands)
└── spur-mentions v1.24.0 (/ws/crates/spur-utilities/mentions)
";

    /// The facade rule failing via a missing family crate.
    const FACADE_TREE_MISSING_MENTIONS: &str = "\
spur-utilities v1.24.0 (/ws/crates/spur-utilities/facade)
└── spur-commands v1.24.0 (/ws/crates/spur-utilities/commands)
";

    /// Verbatim capture of `cargo tree -p xtask --depth 1 -e normal` on a
    /// workspace where xtask has no normal dependencies: a childless root
    /// prints only its own line.
    const CHILDLESS_ROOT_TREE: &str = "xtask v1.24.0 (/ws/xtask)\n";

    /// Trimmed verbatim capture of `cargo tree -p spur-cost -e normal`
    /// (nested levels, `(*)` dedup, and the same crate at both depth 1 and
    /// depth 2 — serde — to pin depth-1-only collection).
    const NESTED_TREE: &str = "\
spur-cost v1.24.0 (/ws/crates/spur-cost)
├── anyhow v1.0.102
├── chrono v0.4.44
│   ├── iana-time-zone v0.1.65
│   │   └── core-foundation-sys v0.8.7
│   ├── num-traits v0.2.19
│   └── serde v1.0.228
│       ├── serde_core v1.0.228
│       └── serde_derive v1.0.228 (proc-macro)
├── rusqlite v0.39.0
│   └── hashlink v0.11.1
├── serde v1.0.228 (*)
└── tracing v0.1.44
";

    /// Trimmed capture of `cargo metadata --no-deps --format-version 1`
    /// (compact JSON: member packages with their dependency arrays).
    const METADATA_SAMPLE: &str = r#"{"packages":[{"name":"spur-acp","version":"1.24.0","id":"path+file:///ws/crates/spur-acp#1.24.0","dependencies":[{"name":"agent-client-protocol","source":"registry+https://github.com/rust-lang/crates.io-index"}]},{"name":"spur-cost","version":"1.24.0","id":"path+file:///ws/crates/spur-cost#1.24.0","dependencies":[{"name":"spur-acp","source":null}]},{"name":"xtask","version":"1.24.0","id":"path+file:///ws/xtask#1.24.0","dependencies":[]}]}"#;

    #[test]
    fn find_forbidden_is_empty_on_clean_default_tree() {
        assert!(find_forbidden(MENTIONS_DEFAULT_TREE, &MENTIONS_DEFAULT_FORBIDDEN).is_empty());
    }

    #[test]
    fn find_forbidden_names_offenders_at_any_depth_in_requested_order() {
        let offenders = find_forbidden(MENTIONS_DEFAULT_TREE_VIOLATING, &["spur-acp", "ratatui"]);

        assert_eq!(offenders, vec!["spur-acp".to_owned(), "ratatui".to_owned()]);
    }

    #[test]
    fn find_forbidden_finds_transitive_acp_in_code_tree() {
        // Documents why check (b) must NOT forbid spur-acp: with `code` the
        // tree contains it by construction (spur-graph -> spur-mcp -> spur-acp).
        assert_eq!(
            find_forbidden(MENTIONS_CODE_TREE, &["spur-acp"]),
            vec!["spur-acp".to_owned()]
        );
        assert!(find_forbidden(MENTIONS_CODE_TREE, &MENTIONS_CODE_FORBIDDEN).is_empty());
    }

    #[test]
    fn find_forbidden_matches_deduped_star_entries() {
        assert_eq!(
            find_forbidden(MENTIONS_DEFAULT_TREE_VIOLATING, &["serde"]),
            vec!["serde".to_owned()]
        );
    }

    #[test]
    fn direct_deps_lists_only_depth_one_children() {
        assert_eq!(
            direct_deps(NESTED_TREE),
            vec![
                "anyhow".to_owned(),
                "chrono".to_owned(),
                "rusqlite".to_owned(),
                "serde".to_owned(),
                "tracing".to_owned(),
            ]
        );
    }

    #[test]
    fn direct_deps_empty_for_childless_root() {
        assert!(direct_deps(CHILDLESS_ROOT_TREE).is_empty());
    }

    #[test]
    fn direct_deps_lists_facade_family_crates() {
        assert_eq!(
            direct_deps(FACADE_TREE_CLEAN),
            vec!["spur-commands".to_owned(), "spur-mentions".to_owned(),]
        );
    }

    #[test]
    fn workspace_has_package_matches_exact_member_names_only() {
        assert!(workspace_has_package(METADATA_SAMPLE, "spur-acp"));
        assert!(workspace_has_package(METADATA_SAMPLE, "xtask"));
        assert!(!workspace_has_package(METADATA_SAMPLE, "spur-mentions"));
        assert!(!workspace_has_package(METADATA_SAMPLE, "spur-utilities"));
        // A name that merely prefixes a member name must not match.
        assert!(!workspace_has_package(METADATA_SAMPLE, "spur-a"));
    }

    #[test]
    fn check_forbidden_absent_passes_clean_trees() {
        check_forbidden_absent(
            "spur-mentions (default features)",
            MENTIONS_DEFAULT_TREE,
            &MENTIONS_DEFAULT_FORBIDDEN,
        )
        .unwrap();
        check_forbidden_absent(
            "spur-mentions (--features code)",
            MENTIONS_CODE_TREE,
            &MENTIONS_CODE_FORBIDDEN,
        )
        .unwrap();
    }

    #[test]
    fn check_forbidden_absent_error_names_offending_crates() {
        let err = check_forbidden_absent(
            "spur-mentions (default features)",
            MENTIONS_DEFAULT_TREE_VIOLATING,
            &MENTIONS_DEFAULT_FORBIDDEN,
        )
        .unwrap_err();

        assert!(err.contains("spur-mentions (default features)"), "{err}");
        assert!(err.contains("spur-acp"), "{err}");
        assert!(err.contains("ratatui"), "{err}");
    }

    #[test]
    fn check_direct_deps_exact_passes_for_facade_fixture() {
        check_direct_deps_exact("spur-utilities", FACADE_TREE_CLEAN, &FACADE_DIRECT_DEPS).unwrap();
    }

    #[test]
    fn check_direct_deps_exact_error_names_unexpected_crate() {
        let err =
            check_direct_deps_exact("spur-utilities", FACADE_TREE_EXTRA_DEP, &FACADE_DIRECT_DEPS)
                .unwrap_err();

        assert!(err.contains("spur-utilities"), "{err}");
        assert!(err.contains("unexpected: serde_json"), "{err}");
    }

    #[test]
    fn check_direct_deps_exact_error_names_missing_crate() {
        let err = check_direct_deps_exact(
            "spur-utilities",
            FACADE_TREE_MISSING_MENTIONS,
            &FACADE_DIRECT_DEPS,
        )
        .unwrap_err();

        assert!(err.contains("spur-utilities"), "{err}");
        assert!(err.contains("missing: spur-mentions"), "{err}");
    }

    #[test]
    fn cargo_tree_commands_carry_the_spec_scopes() {
        let root = PathBuf::from("/ws");

        let default_tree = cargo_tree_command(&root, &["-p", MENTIONS_PACKAGE]);
        assert_eq!(
            command_args(&default_tree),
            vec![
                "tree".to_owned(),
                "-p".to_owned(),
                "spur-mentions".to_owned(),
                "-e".to_owned(),
                "normal".to_owned(),
            ]
        );

        let code_tree = cargo_tree_command(&root, &["-p", MENTIONS_PACKAGE, "--features", "code"]);
        assert_eq!(
            command_args(&code_tree),
            vec![
                "tree".to_owned(),
                "-p".to_owned(),
                "spur-mentions".to_owned(),
                "--features".to_owned(),
                "code".to_owned(),
                "-e".to_owned(),
                "normal".to_owned(),
            ]
        );

        let facade_tree = cargo_tree_command(&root, &["-p", FACADE_PACKAGE, "--depth", "1"]);
        assert_eq!(
            command_args(&facade_tree),
            vec![
                "tree".to_owned(),
                "-p".to_owned(),
                "spur-utilities".to_owned(),
                "--depth".to_owned(),
                "1".to_owned(),
                "-e".to_owned(),
                "normal".to_owned(),
            ]
        );

        for cmd in [default_tree, code_tree, facade_tree] {
            assert_eq!(cmd.get_current_dir(), Some(root.as_path()));
        }
    }

    #[test]
    fn cargo_metadata_command_is_no_deps_format_one() {
        let root = PathBuf::from("/ws");

        let cmd = cargo_metadata_command(&root);

        assert_eq!(
            command_args(&cmd),
            vec![
                "metadata".to_owned(),
                "--no-deps".to_owned(),
                "--format-version".to_owned(),
                "1".to_owned(),
            ]
        );
        assert_eq!(cmd.get_current_dir(), Some(root.as_path()));
    }
}
