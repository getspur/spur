//! Backward-compat and round-trip coverage for the `[tui]` config section.

use spur_acp::config::{EditorMode, SpurConfig};

#[test]
fn parse_config_without_tui_section_uses_emacs_default() {
    // Old config, predating the [tui] section. Must parse cleanly and yield
    // EditorMode::Emacs so existing users see zero behavior change.
    let toml = r#"
        peer_mailbox_enabled = false

        [brain]
        default = "claude-code"

        [agents]
    "#;
    let cfg: SpurConfig = toml::from_str(toml).expect("old config must parse");
    assert_eq!(cfg.tui.edit_mode, EditorMode::Emacs);
}

#[test]
fn roundtrip_tui_edit_mode_vim_preserves_value() {
    let toml = r#"
        [tui]
        edit_mode = "vim"
    "#;
    let cfg: SpurConfig = toml::from_str(toml).expect("must parse");
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);

    let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
    let cfg2: SpurConfig = toml::from_str(&serialized).expect("must reparse");
    assert_eq!(cfg2.tui.edit_mode, EditorMode::Vim);
}

#[test]
fn roundtrip_tui_disable_paste_burst_preserves_value() {
    let toml = r#"
        [tui]
        disable_paste_burst = true
    "#;
    let cfg: SpurConfig = toml::from_str(toml).expect("must parse");
    assert!(cfg.tui.disable_paste_burst);

    let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
    let cfg2: SpurConfig = toml::from_str(&serialized).expect("must reparse");
    assert!(cfg2.tui.disable_paste_burst);
}

#[test]
fn default_tui_config_is_skipped_on_serialize() {
    // Default TuiConfig must NOT emit a [tui] block — keeps existing user
    // configs visually unchanged after a round-trip through `spur init`.
    let cfg = SpurConfig::default();
    let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
    assert!(
        !serialized.contains("[tui]"),
        "default config must not emit [tui] section, got:\n{serialized}"
    );
}

#[test]
fn parse_config_without_tui_section_uses_dark_theme_default() {
    let toml = r#"
        peer_mailbox_enabled = false

        [brain]
        default = "claude-code"

        [agents]
    "#;
    let cfg: SpurConfig = toml::from_str(toml).expect("old config must parse");
    assert_eq!(cfg.tui.theme, "dark");
}

#[test]
fn roundtrip_tui_theme_light_preserves_value() {
    let toml = r#"
        [tui]
        theme = "light"
    "#;
    let cfg: SpurConfig = toml::from_str(toml).expect("must parse");
    assert_eq!(cfg.tui.theme, "light");

    let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
    let cfg2: SpurConfig = toml::from_str(&serialized).expect("must reparse");
    assert_eq!(cfg2.tui.theme, "light");
}

#[test]
fn invalid_edit_mode_value_fails_to_parse() {
    let toml = r#"
        [tui]
        edit_mode = "wim"
    "#;
    let result: Result<SpurConfig, _> = toml::from_str(toml);
    assert!(result.is_err(), "invalid value must fail to parse");
}
