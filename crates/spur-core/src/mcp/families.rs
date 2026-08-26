//! Worker MCP family selection for `spur exec --enable-mcp`.
//!
//! SOLVE PRE (configuration family):
//! - default-all 9 leaves: `sol_750d8ebbe59e4038` (`pass`)
//! - `alias_core` without `family_pm`: `sol_e5dd5bb343ac4ba0` (`fail`,
//!   `configuration.requires_any.violation`)
//! SOLVE POST (landed `McpFamily::ALL` len 9):
//! - default-all: `sol_60f41f6973ca4478` (`pass`)
//! - solver-only: `sol_c7aceaac714948e1` (`pass`)
//! - `alias_core` without pm: `sol_ac2be290fc124829` (`fail`)
//! Leaf cardinality `maximum` in that encoding is 9.

use std::collections::BTreeSet;
use std::fmt;

/// One selectable worker MCP leaf. Aliases (`all`, `core`, `code`) expand
/// into these leaves; they are not themselves leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpFamily {
    Graph,
    Context,
    Analyst,
    Solver,
    Pm,
    Skills,
    Signals,
    Worker,
    Review,
}

impl McpFamily {
    pub const ALL: [Self; 9] = [
        Self::Graph,
        Self::Context,
        Self::Analyst,
        Self::Solver,
        Self::Pm,
        Self::Skills,
        Self::Signals,
        Self::Worker,
        Self::Review,
    ];

    pub const CORE: [Self; 7] = [
        Self::Analyst,
        Self::Solver,
        Self::Pm,
        Self::Skills,
        Self::Signals,
        Self::Worker,
        Self::Review,
    ];

    pub const CODE: [Self; 2] = [Self::Graph, Self::Analyst];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::Context => "context",
            Self::Analyst => "analyst",
            Self::Solver => "solver",
            Self::Pm => "pm",
            Self::Skills => "skills",
            Self::Signals => "signals",
            Self::Worker => "worker",
            Self::Review => "review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpFamilyError {
    Unknown { name: String },
    Empty,
}

impl fmt::Display for McpFamilyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown { name } => write!(
                f,
                "unknown MCP family `{name}`; expected one of: {}",
                known_tokens().join(", ")
            ),
            Self::Empty => write!(
                f,
                "MCP family selection resolved to an empty set; enable at least one family"
            ),
        }
    }
}

impl std::error::Error for McpFamilyError {}

fn known_tokens() -> Vec<&'static str> {
    let mut names = vec!["all", "core", "code"];
    names.extend(McpFamily::ALL.iter().map(|family| family.as_str()));
    names
}

/// Expand-then-subtract family resolution.
///
/// - No `--enable-mcp` and no family flags → `Ok(None)` (historical no-MCP).
/// - `--enable-mcp` and no family flags → all 9 leaves.
/// - Family flags without `--enable-mcp` imply a session.
/// - `--mcp-family` expands aliases/leaves; `--disable-mcp-family` subtracts.
pub fn resolve_mcp_families<S: AsRef<str>>(
    enable_mcp: bool,
    enable: &[S],
    disable: &[S],
) -> Result<Option<BTreeSet<McpFamily>>, McpFamilyError> {
    if !enable_mcp && enable.is_empty() && disable.is_empty() {
        return Ok(None);
    }
    let mut leaves = if enable.is_empty() {
        McpFamily::ALL.into_iter().collect()
    } else {
        expand_tokens(enable)?
    };
    for family in expand_tokens(disable)? {
        leaves.remove(&family);
    }
    if leaves.is_empty() {
        return Err(McpFamilyError::Empty);
    }
    Ok(Some(leaves))
}

fn expand_tokens<S: AsRef<str>>(tokens: &[S]) -> Result<BTreeSet<McpFamily>, McpFamilyError> {
    let mut leaves = BTreeSet::new();
    for token in tokens {
        let name = token.as_ref().trim();
        if name.is_empty() {
            continue;
        }
        match name {
            "all" => leaves.extend(McpFamily::ALL),
            "core" => leaves.extend(McpFamily::CORE),
            "code" => leaves.extend(McpFamily::CODE),
            other => {
                let family = McpFamily::ALL
                    .iter()
                    .copied()
                    .find(|family| family.as_str() == other)
                    .ok_or_else(|| McpFamilyError::Unknown {
                        name: other.to_owned(),
                    })?;
                leaves.insert(family);
            }
        }
    }
    Ok(leaves)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(enable_mcp: bool, enable: &[&str], disable: &[&str]) -> BTreeSet<&'static str> {
        resolve_mcp_families(enable_mcp, enable, disable)
            .expect("selection should resolve")
            .expect("MCP session should be on")
            .into_iter()
            .map(McpFamily::as_str)
            .collect()
    }

    #[test]
    fn mcp_off_with_no_family_flags_is_none() {
        assert_eq!(
            resolve_mcp_families(false, &[] as &[&str], &[] as &[&str]).unwrap(),
            None
        );
    }

    #[test]
    fn enable_mcp_defaults_to_all_nine_leaves() {
        // Witness: sol_750d8ebbe59e4038 — leaves group maximum 9, all selected.
        let selected = names(true, &[], &[]);
        assert_eq!(selected.len(), 9);
        for family in McpFamily::ALL {
            assert!(selected.contains(family.as_str()), "{}", family.as_str());
        }
    }

    #[test]
    fn solver_only_allowlist() {
        let selected = names(true, &["solver"], &[]);
        assert_eq!(selected, BTreeSet::from(["solver"]));
    }

    #[test]
    fn family_flags_imply_mcp_session() {
        let selected = names(false, &["graph", "solver"], &[]);
        assert_eq!(selected, BTreeSet::from(["graph", "solver"]));
    }

    #[test]
    fn core_alias_expands_to_seven_leaves() {
        let selected = names(true, &["core"], &[]);
        assert_eq!(
            selected,
            McpFamily::CORE
                .iter()
                .map(|family| family.as_str())
                .collect()
        );
        assert!(!selected.contains("graph"));
        assert!(!selected.contains("context"));
    }

    #[test]
    fn expand_core_then_subtract_pm() {
        // Witness: sol_e5dd5bb343ac4ba0 — claiming alias_core while pm is off
        // is unsat. After subtract, the stored selection is the remaining
        // core leaves, not alias_core.
        let selected = names(true, &["core"], &["pm"]);
        assert!(!selected.contains("pm"));
        assert_eq!(selected.len(), 6);
        for family in ["analyst", "solver", "skills", "signals", "worker", "review"] {
            assert!(selected.contains(family), "{family}");
        }
    }

    #[test]
    fn disable_pm_from_default_all() {
        let selected = names(true, &[], &["pm"]);
        assert_eq!(selected.len(), 8);
        assert!(!selected.contains("pm"));
    }

    #[test]
    fn code_alias_is_graph_and_analyst() {
        let selected = names(true, &["code"], &[]);
        assert_eq!(selected, BTreeSet::from(["analyst", "graph"]));
    }

    #[test]
    fn unknown_family_is_rejected() {
        let err = resolve_mcp_families(true, &["notebook"], &[] as &[&str]).unwrap_err();
        assert!(matches!(err, McpFamilyError::Unknown { ref name } if name == "notebook"));
    }

    #[test]
    fn empty_resolved_set_is_rejected() {
        let err = resolve_mcp_families(true, &["solver"], &["solver"]).unwrap_err();
        assert_eq!(err, McpFamilyError::Empty);
    }
}
