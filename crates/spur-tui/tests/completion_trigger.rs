use spur_tui::components::completion_trigger::{detect, TriggerKind};

#[test]
fn slash_at_offset_zero_opens_slash_trigger() {
    let t = detect("/he", 3).expect("slash trigger");
    assert_eq!(t.kind, TriggerKind::Slash);
    assert_eq!(t.query, "he");
    assert_eq!(t.prefix_start, 0);
}

#[test]
fn slash_after_whitespace_does_not_trigger_in_v1() {
    assert!(detect("hello /foo", 10).is_none());
}

#[test]
fn at_after_whitespace_opens_mention_trigger() {
    let t = detect("look at @sr", 11).expect("mention trigger");
    assert_eq!(t.kind, TriggerKind::Mention);
    assert_eq!(t.query, "sr");
    assert_eq!(t.prefix_start, 8);
}

#[test]
fn at_at_offset_zero_opens_mention_trigger() {
    let t = detect("@foo", 4).expect("mention trigger");
    assert_eq!(t.kind, TriggerKind::Mention);
    assert_eq!(t.query, "foo");
}

#[test]
fn mention_closes_on_space() {
    assert!(detect("look at @foo bar", 16).is_none());
}

#[test]
fn cursor_before_trigger_means_no_trigger() {
    assert!(detect("look at @foo", 0).is_none());
}

#[test]
fn empty_query_after_trigger() {
    let t = detect("/", 1).expect("empty-query trigger");
    assert_eq!(t.kind, TriggerKind::Slash);
    assert_eq!(t.query, "");
}
