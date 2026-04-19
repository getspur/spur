//! Defense-in-depth: a corrupted or hand-edited session_metadata.json
//! must not produce InputStateSnapshots with invalid ProtectedRanges.
//! Invalid ranges are dropped on load; the text payload is preserved.

use spur_tui::input_history::InputStateSnapshot;

fn snapshot_json(text: &str, ranges_json: &str) -> String {
    format!(
        r#"{{"text": {text:?}, "protected_ranges": {ranges_json}}}"#,
        text = text
    )
}

#[test]
fn deserialize_accepts_well_formed_snapshot() {
    let json = snapshot_json(
        "hello @foo",
        r#"[{"start": 6, "end": 10, "uri": "file:///foo", "name": "foo"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.text, "hello @foo");
    assert_eq!(snap.protected_ranges.len(), 1);
}

#[test]
fn deserialize_drops_range_with_end_past_text_len() {
    let json = snapshot_json(
        "short",
        r#"[{"start": 0, "end": 999, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.text, "short");
    assert!(
        snap.protected_ranges.is_empty(),
        "invalid range must be dropped, not preserved"
    );
}

#[test]
fn deserialize_drops_range_with_start_after_end() {
    let json = snapshot_json(
        "hello world",
        r#"[{"start": 5, "end": 2, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert!(snap.protected_ranges.is_empty());
}

#[test]
fn deserialize_drops_range_off_char_boundary() {
    // "héllo" — 'é' is two bytes (U+00E9 = 0xC3 0xA9) starting at index 1.
    // Index 2 is mid-codepoint and must be rejected.
    let json = snapshot_json(
        "héllo",
        r#"[{"start": 1, "end": 2, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert!(
        snap.protected_ranges.is_empty(),
        "non-char-boundary range must be dropped"
    );
}

#[test]
fn deserialize_drops_overlapping_ranges_keeping_first() {
    let json = snapshot_json(
        "aaaaaaaa",
        r#"[
            {"start": 0, "end": 4, "uri": "file:///a", "name": "a"},
            {"start": 2, "end": 6, "uri": "file:///b", "name": "b"}
        ]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.protected_ranges.len(), 1);
    assert_eq!(snap.protected_ranges[0].name, "a");
}

#[test]
fn deserialize_sorts_ranges_by_start() {
    let json = snapshot_json(
        "aaaaaaaaaa",
        r#"[
            {"start": 6, "end": 8, "uri": "file:///b", "name": "b"},
            {"start": 0, "end": 2, "uri": "file:///a", "name": "a"}
        ]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.protected_ranges.len(), 2);
    assert_eq!(snap.protected_ranges[0].name, "a");
    assert_eq!(snap.protected_ranges[1].name, "b");
}

#[test]
fn deserialize_preserves_text_when_all_ranges_invalid() {
    let json = snapshot_json(
        "@foo bar",
        r#"[{"start": 99, "end": 999, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.text, "@foo bar");
    assert!(snap.protected_ranges.is_empty());
}
