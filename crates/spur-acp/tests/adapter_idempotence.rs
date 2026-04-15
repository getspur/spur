//! Idempotence tests for per-kind `refine` functions.
//!
//! Property: for any `(title, base)` pair,
//!   `refine(title, refine(title, base)) == refine(title, base)`
//!
//! Tested for Claude, Codex, Kiro, and Generic across a corpus of ~6 titles.

use spur_acp::adapter::{claude, codex, generic, kiro, ToolFamily};

/// The title corpus covers: empty, a known ACP-mapped name, the MCP prefix,
/// a Claude-specific tool, a Codex-specific tool, and an unrecognised name.
const TITLES: &[&str] = &[
    "",
    "Read",
    "mcp__some_server__tool",
    "TodoWrite",
    "plan_update",
    "WeirdThing",
];

/// All base families that a caller might pass into `refine`.
const BASES: &[ToolFamily] = &[
    ToolFamily::Read,
    ToolFamily::Edit,
    ToolFamily::Delete,
    ToolFamily::Move,
    ToolFamily::Search,
    ToolFamily::Execute,
    ToolFamily::Think,
    ToolFamily::Fetch,
    ToolFamily::SwitchMode,
    ToolFamily::Plan,
    ToolFamily::Mcp,
    ToolFamily::Unknown,
];

#[test]
fn claude_refine_is_idempotent() {
    for title in TITLES {
        for &base in BASES {
            let once = claude::refine(title, base);
            let twice = claude::refine(title, once);
            assert_eq!(
                once, twice,
                "claude::refine idempotence failed for title={title:?} base={base:?}"
            );
        }
    }
}

#[test]
fn codex_refine_is_idempotent() {
    for title in TITLES {
        for &base in BASES {
            let once = codex::refine(title, base);
            let twice = codex::refine(title, once);
            assert_eq!(
                once, twice,
                "codex::refine idempotence failed for title={title:?} base={base:?}"
            );
        }
    }
}

#[test]
fn kiro_refine_is_idempotent() {
    for title in TITLES {
        for &base in BASES {
            let once = kiro::refine(title, base);
            let twice = kiro::refine(title, once);
            assert_eq!(
                once, twice,
                "kiro::refine idempotence failed for title={title:?} base={base:?}"
            );
        }
    }
}

#[test]
fn generic_refine_is_idempotent() {
    for title in TITLES {
        for &base in BASES {
            let once = generic::refine(title, base);
            let twice = generic::refine(title, once);
            assert_eq!(
                once, twice,
                "generic::refine idempotence failed for title={title:?} base={base:?}"
            );
        }
    }
}
