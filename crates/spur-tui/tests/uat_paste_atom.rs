mod common;

use crossterm::event::{KeyCode, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::components::input_bar::RangeKind;
use spur_tui::UserInput;

use common::TestHarness;

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => text.text.as_str(),
            _ => "",
        })
        .collect::<Vec<_>>()
        .join("")
}

fn submitted_text_and_interrupt(action: UserInput) -> (String, bool) {
    match action {
        UserInput::Message {
            blocks, interrupt, ..
        }
        | UserInput::NewSessionWithMessage { blocks, interrupt } => {
            (blocks_text(&blocks), interrupt)
        }
        other => panic!("expected message submission, got {}", action_kind(&other)),
    }
}

fn action_kind(action: &UserInput) -> &'static str {
    match action {
        UserInput::Message { .. } => "Message",
        UserInput::NewSessionWithMessage { .. } => "NewSessionWithMessage",
        UserInput::ListSessions => "ListSessions",
        UserInput::ResumeSession { .. } => "ResumeSession",
        UserInput::SetSessionMode { .. } => "SetSessionMode",
        UserInput::SubmitReview { .. } => "SubmitReview",
        UserInput::VendorExec { .. } => "VendorExec",
        UserInput::CancelStream { .. } => "CancelStream",
        UserInput::RefreshIssues => "RefreshIssues",
        UserInput::RefreshPlans => "RefreshPlans",
        UserInput::ClaimPlan { .. } => "ClaimPlan",
        UserInput::ForceReclaimPlan { .. } => "ForceReclaimPlan",
        UserInput::ResumePlan { .. } => "ResumePlan",
        UserInput::InspectPlan { .. } => "InspectPlan",
        UserInput::GetIssueDetail { .. } => "GetIssueDetail",
        UserInput::GetIssueGraph { .. } => "GetIssueGraph",
        UserInput::UpdateIssue { .. } => "UpdateIssue",
        UserInput::SetSessionConfigOption { .. } => "SetSessionConfigOption",
        UserInput::SetSessionModel { .. } => "SetSessionModel",
    }
}

#[test]
fn f3_u1_multiline_paste_atomizes_in_input_bar() {
    let mut h = TestHarness::new(80, 24);

    h.render();
    h.send_paste("fn main() {\n    println!(\"hello\");\n}");
    h.render();

    let text = h.buffer_text();
    assert!(
        text.contains("[Paste #1 · 3 lines]"),
        "expected paste placeholder visible, got buffer:\n{text}"
    );

    let bar = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_mut_for_test();
    assert_eq!(bar.protected_ranges().len(), 1);
    assert_eq!(bar.protected_ranges()[0].kind, RangeKind::PasteRef(1));
}

#[test]
fn f3_u2_submit_emits_full_text_action() {
    let mut h = TestHarness::new(80, 24);
    let pasted = (1..=16)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");

    h.send_paste(&pasted);
    h.send_key(KeyCode::Enter);

    let action = h.last_action().expect("outbound user input");
    let (text, interrupt) = submitted_text_and_interrupt(action);
    assert_eq!(text, pasted);
    assert!(!interrupt);
}

#[test]
fn f3_u3_bang_prefix_paste_propagates_interrupt() {
    let mut h = TestHarness::new(80, 24);
    let pasted = "!stop\nplease halt";

    h.send_paste(pasted);
    h.send_key(KeyCode::Enter);

    let action = h.last_action().expect("outbound user input");
    let (text, interrupt) = submitted_text_and_interrupt(action);
    assert_eq!(text, pasted);
    assert!(interrupt);
}

#[test]
fn f3_u4_referenced_pastes_are_retained_over_cap() {
    let mut h = TestHarness::new(80, 24);

    for i in 1..=55 {
        h.send_paste(&format!("paste {i}\nline 2"));
    }

    let bar = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_mut_for_test();
    let keys = bar.paste_ids_for_test();
    assert_eq!(keys.len(), 55);
    for retained in 1..=55 {
        assert!(
            keys.contains(&retained),
            "paste id {retained} should be retained; keys={keys:?}"
        );
    }
}

#[test]
fn f3_u5_paste_during_history_browse_restores_draft() {
    let mut h = TestHarness::new(80, 24);

    h.send_paste("old\nmessage");
    h.send_key(KeyCode::Enter);
    let _ = h.take_actions();

    h.type_text("new draft");
    h.send_key_with_mods(KeyCode::Char('p'), KeyModifiers::CONTROL);
    h.send_paste("interrupting\npaste");
    h.render();

    let bar = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_mut_for_test();
    assert!(
        bar.text().contains("new draft"),
        "draft should be restored before paste, got {:?}",
        bar.text()
    );
    assert!(
        bar.text().contains("[Paste #2 · 2 lines]"),
        "second paste placeholder should be appended, got {:?}",
        bar.text()
    );
    assert!(
        bar.history_cursor_for_test().is_none(),
        "paste should reset history browse state"
    );
}

#[test]
fn f3_u6_paste_with_carriage_return_separators_normalizes_at_app_boundary() {
    let mut h = TestHarness::new(80, 24);

    // Mac-legacy clipboard: bare \r between lines.
    h.send_paste("a\rb\rc");

    let bar = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_mut_for_test();
    assert_eq!(bar.text(), "[Paste #1 · 3 lines]");
}

#[test]
fn f3_u7_paste_with_crlf_separators_normalizes_at_app_boundary() {
    let mut h = TestHarness::new(80, 24);

    // Windows clipboard: \r\n between lines.
    h.send_paste("a\r\nb\r\nc");

    let bar = h
        .app_mut()
        .dashboard_mut_for_test()
        .input_bar_mut_for_test();
    assert_eq!(bar.text(), "[Paste #1 · 3 lines]");
}
