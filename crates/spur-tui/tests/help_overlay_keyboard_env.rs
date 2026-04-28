use spur_tui::components::help_overlay::HelpOverlay;

fn help_text() -> String {
    HelpOverlay::lines(false, false)
        .into_iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn help_overlay_mentions_keyboard_environment_caveats() {
    let text = help_text();

    assert!(text.contains("Keyboard environment"), "{text}");
    assert!(text.contains("Use Option as Meta"), "{text}");
    assert!(text.contains("tmux"), "{text}");
    assert!(text.contains("stty ixon"), "{text}");
}
