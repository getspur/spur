#![cfg(feature = "duckdb")]
//! These fixtures exercise real DuckDB query paths via `AnalyticsEngine::conn()`,
//! which only exists when the `duckdb` feature is enabled. Skip the whole
//! compilation unit otherwise — the test is meaningless against the no-op stub.

use anyhow::Result;
use spur_context::AnalyticsEngine;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

#[test]
fn real_fixtures_exercise_heterogeneous_views() -> Result<()> {
    let temp = tempfile::tempdir()?;

    let claude_root = temp.path().join("claude-root");
    let claude_target = claude_root.join("projects/anon-proj/fixture.jsonl");
    fs::create_dir_all(claude_target.parent().expect("claude fixture parent"))?;
    fs::copy(
        repo_fixture("tests/fixtures/real/claude_heterogeneous.jsonl"),
        &claude_target,
    )?;

    let codex_root = temp.path().join("codex-root");
    let codex_target = codex_root.join("sessions/2026/04/15/fixture.jsonl");
    fs::create_dir_all(codex_target.parent().expect("codex fixture parent"))?;
    fs::copy(
        repo_fixture("tests/fixtures/real/codex_heterogeneous.jsonl"),
        &codex_target,
    )?;

    let kiro_root = temp.path().join("kiro-root");
    fs::create_dir_all(&kiro_root)?;

    env::set_var("CLAUDE_CONFIG_DIR", &claude_root);
    env::set_var("CODEX_HOME", &codex_root);
    env::set_var("KIRO_HOME", &kiro_root);

    let engine = AnalyticsEngine::open_in_memory()?;
    engine.initialize()?;
    let status = engine.create_agent_views()?;

    assert!(status.claude, "claude_events view should be created");
    assert!(status.codex, "codex_events view should be created");

    let claude_count: i64 =
        engine
            .conn()
            .query_row("SELECT COUNT(*) FROM claude_events", [], |row| row.get(0))?;
    assert_eq!(
        claude_count, 2,
        "after dedup: two rows share requestId+message.id (collapsed to 1) + one unique = 2"
    );

    let raw_assistant_count: i64 = engine.conn().query_row(
        "SELECT COUNT(*) FROM claude_raw
         WHERE json_valid(line)
           AND json_extract_string(line, '$.type') = 'assistant'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        raw_assistant_count, 3,
        "fixture has 3 assistant rows; dedup must reduce claude_events to 2"
    );

    let codex_count: i64 =
        engine
            .conn()
            .query_row("SELECT COUNT(*) FROM codex_events", [], |row| row.get(0))?;
    assert_eq!(codex_count, 2, "zero-delta codex rows should be filtered");

    let (claude_input_sum, claude_output_sum): (Option<i64>, Option<i64>) =
        engine.conn().query_row(
            "SELECT SUM(input_tokens), SUM(output_tokens) FROM claude_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    // After dedup: earlier-timestamp row of the duplicate pair (input=6, output=8)
    // survives; the distinct third row contributes (input=7, output=100).
    assert_eq!(claude_input_sum, Some(13));
    assert_eq!(claude_output_sum, Some(108));

    let codex_session_id: String =
        engine
            .conn()
            .query_row("SELECT DISTINCT session_id FROM codex_events", [], |row| {
                row.get(0)
            })?;
    assert_eq!(codex_session_id, "fixture");

    let mut stmt = engine
        .conn()
        .prepare("SELECT DISTINCT model FROM codex_events ORDER BY model")?;
    let models: Vec<Option<String>> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(
        models
            .iter()
            .any(|model| model.as_deref() == Some("gpt-5.4")),
        "turn_context model should carry into codex token rows"
    );

    Ok(())
}
