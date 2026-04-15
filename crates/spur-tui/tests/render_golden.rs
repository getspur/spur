/// Golden-file rendering tests for the react_trace pipeline.
///
/// Each test pushes hand-crafted `TraceEntry` values directly into a
/// `ReactTrace` and calls `render_lines_for_test(80)` to get a Vec<String>.
/// The joined output is compared against a committed `.txt` golden file.
///
/// Re-record with: `UPDATE_GOLDEN=1 cargo test -p spur-tui --test render_golden`
use spur_acp::{
    adapter::{ObservePayload, ToolFamily, ToolInputDisplay},
    AgentKind,
};
use spur_tui::components::react_trace::{ReactTrace, TraceEntry, TraceKind};

fn make_trace(kind: AgentKind) -> ReactTrace {
    ReactTrace::with_kind(kind)
}

fn push_act(
    trace: &mut ReactTrace,
    tool: &str,
    family: ToolFamily,
    input: ToolInputDisplay,
    fallback_text: &str,
) {
    trace.push(TraceEntry {
        kind: TraceKind::Act {
            tool: tool.to_string(),
            family,
            input,
        },
        text: fallback_text.to_string(),
        timestamp: "12:00:00".to_string(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
}

fn push_observe(trace: &mut ReactTrace, payload: Option<ObservePayload>, fallback_text: &str) {
    trace.push(TraceEntry {
        kind: TraceKind::Observe { payload },
        text: fallback_text.to_string(),
        timestamp: "12:00:01".to_string(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
}

fn render_to_string(trace: &ReactTrace) -> String {
    trace.render_to_strings().join("\n")
}

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name)
}

fn check_or_update(actual: &str, golden_name: &str) {
    let path = golden_path(golden_name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, actual).expect("write golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "golden file not found: {}; run with UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "golden mismatch for {}; re-record with UPDATE_GOLDEN=1",
        golden_name
    );
}

// ─── Test 1: claude edit ────────────────────────────────────────────────────

#[test]
fn claude_edit_renders_golden() {
    let mut trace = make_trace(AgentKind::ClaudeCodeAcp);

    // ACT: an Edit tool call with a diff input
    push_act(
        &mut trace,
        "Edit",
        ToolFamily::Edit,
        ToolInputDisplay::Diff {
            path: "/tmp/foo.rs".to_string(),
            diff: "-a\n+b\n".to_string(),
        },
        "",
    );

    // OBSERVE: edit result
    push_observe(
        &mut trace,
        Some(ObservePayload::EditResult {
            path: Some("/tmp/foo.rs".to_string()),
            replacements: Some(1),
            diff: None,
        }),
        "",
    );

    let actual = render_to_string(&trace);
    check_or_update(&actual, "claude_edit.txt");
}

// ─── Test 2: codex exec exit0 ───────────────────────────────────────────────

#[test]
fn codex_exec_exit0_renders_golden() {
    let mut trace = make_trace(AgentKind::CodexAcp);

    // ACT: an Execute tool call
    push_act(
        &mut trace,
        "exec_command",
        ToolFamily::Execute,
        ToolInputDisplay::Command {
            cmd: "cargo build".to_string(),
            cwd: None,
        },
        "",
    );

    // OBSERVE: exit 0
    push_observe(
        &mut trace,
        Some(ObservePayload::CommandOutput {
            exit_code: Some(0),
            stdout: "done\n".to_string(),
            stderr: String::new(),
        }),
        "",
    );

    let actual = render_to_string(&trace);

    // Sanity assertions: must contain exit 0 summary and NOT contain raw JSON
    assert!(
        actual.contains("exit 0"),
        "expected '✓ exit 0' in codex_exec output, got:\n{actual}"
    );
    assert!(
        !actual.contains(r#"{"items":["#),
        "raw JSON envelope must not appear in rendered output, got:\n{actual}"
    );

    check_or_update(&actual, "codex_exec_exit0.txt");
}

// ─── Test 3: generic MCP envelope ───────────────────────────────────────────

#[test]
fn generic_mcp_envelope_renders_golden() {
    // This is the user-pasted-sample pattern:
    //   {"items":[{"Json":{"exit_code":0,"stdout":"done\n"}}]}
    // After MCP unwrap + generic extract → CommandOutput { exit_code: Some(0), stdout: "done\n" }
    // Rendered as: "✓ ran" header + "$ exit 0" + "done" body

    let mut trace = make_trace(AgentKind::Generic);

    // Push an MCP tool call (already classified as Mcp family)
    push_act(
        &mut trace,
        "mcp__server__run_tool",
        ToolFamily::Mcp,
        ToolInputDisplay::Empty,
        "",
    );

    // The observe payload as the adapter would produce after MCP unwrap:
    // {"items":[{"Json":{"exit_code":0,"stdout":"done\n"}}]}
    // → unwrap → {"exit_code":0,"stdout":"done\n"}
    // → generic extract → CommandOutput
    push_observe(
        &mut trace,
        Some(ObservePayload::CommandOutput {
            exit_code: Some(0),
            stdout: "done\n".to_string(),
            stderr: String::new(),
        }),
        "",
    );

    let actual = render_to_string(&trace);

    // Must contain "✓ exit 0" and "done"; must NOT contain raw JSON envelope
    assert!(
        actual.contains("exit 0"),
        "expected exit 0 in mcp_envelope output, got:\n{actual}"
    );
    assert!(
        actual.contains("done"),
        "expected 'done' stdout in mcp_envelope output, got:\n{actual}"
    );
    assert!(
        !actual.contains(r#"{"items":["#),
        "raw JSON envelope must not appear, got:\n{actual}"
    );

    check_or_update(&actual, "generic_mcp_envelope.txt");
}
