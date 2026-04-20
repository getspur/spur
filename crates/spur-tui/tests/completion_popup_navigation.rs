use spur_tui::components::completion_popup::{CompletionPopup, PopupRow};

#[test]
fn completion_popup_select_cycles() {
    let mut p = CompletionPopup::new();
    p.set_rows(vec![
        PopupRow {
            label: "/help".into(),
            description: "".into(),
            source_tag: "⟨spur⟩".into(),
        },
        PopupRow {
            label: "/compact".into(),
            description: "".into(),
            source_tag: "⟨claude⟩".into(),
        },
    ]);
    assert_eq!(p.selected(), Some(0));
    p.select_next();
    assert_eq!(p.selected(), Some(1));
    p.select_next();
    assert_eq!(p.selected(), Some(0));
    p.select_prev();
    assert_eq!(p.selected(), Some(1));
}
