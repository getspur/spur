use spur_bot::telegram::format::{short_button_label, split_for_telegram};

#[test]
fn split_for_telegram_preserves_unicode_scalar_boundaries() {
    let text = "alpha🙂beta🙂gamma".repeat(400);
    let chunks = split_for_telegram(&text, 256);

    assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 256));
    assert_eq!(chunks.concat(), text);
}

#[test]
fn short_button_label_keeps_action_verb() {
    assert_eq!(short_button_label("Allow Once", 12), "Allow Once");
    assert_eq!(
        short_button_label("Allow Always for This Tool", 16),
        "Allow Always"
    );
}
