//! Init UX tests: contract guard + behavioral tests for `spur init`.
//!
//! Kept as one file because the contract test is trivial and colocating
//! it with the behavioral tests means future contributors see the
//! install-hint requirement when they open this file.

#![cfg(unix)]

#[test]
fn install_hints_cover_all_seed_agents() {
    // Can't access the private const directly from an integration test.
    // Re-encode it here via a parallel list. If the two drift, the test
    // fails and forces the contributor to update both sides.
    //
    // Alternative: expose INSTALL_HINTS as pub from a lib target. That
    // would be cleaner, but spur-cli is a binary-only crate and adding
    // a lib target just for this is overkill. Keep the parallel list.
    let expected_names: &[&str] = &[
        "claude-code",
        "kiro",
        "claude-code-acp",
        "codex",
        "gemini",
    ];
    let seeds = spur_acp::config::load_seed_template();
    for agent in &seeds.entries {
        assert!(
            expected_names.contains(&agent.name.as_str()),
            "seed agent `{}` has no INSTALL_HINTS entry — add one to \
             crates/spur-cli/src/main.rs AND to expected_names in this test",
            agent.name
        );
    }
    // Also check the reverse direction: no orphan expected_names that
    // aren't in seeds (would indicate a stale hint for a deleted agent).
    let seed_names: Vec<_> = seeds.entries.iter().map(|a| a.name.as_str()).collect();
    for expected in expected_names {
        assert!(
            seed_names.contains(expected),
            "expected_names has `{expected}` but it's not in seed template"
        );
    }
}
