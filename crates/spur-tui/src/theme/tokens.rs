use super::palette::{Palette, PaletteEntry};
use std::collections::HashMap;

const DARK_DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("status_bar.bg", "bg_panel"),
    ("status_bar.fg", "fg"),
    ("border.normal", "border"),
    ("border.focused", "border_focused"),
    ("spinner.fg", "accent"),
    ("picker.selected.bg", "bg_selection"),
    ("picker.selected.fg", "fg"),
    ("picker.match.fg", "highlight"),
    ("picker.hint.fg", "fg_subtle"),
    ("status_bar.tombstone.fg", "fg_subtle"),
    ("status_bar.issue_count.fg", "accent"),
    ("status_bar.separator.fg", "fg_subtle"),
    ("tool.family.thinking", "fg_subtle"),
    ("tool.family.edit", "warning"),
    ("tool.family.read", "accent"),
    ("tool.family.delete", "danger"),
    ("tool.family.move", "warning"),
    ("tool.family.search", "info"),
    ("tool.family.bash", "accent_alt"),
    ("tool.family.fetch", "info"),
    ("tool.family.switch_mode", "accent"),
    ("tool.family.task", "accent"),
    ("tool.family.mcp", "fg_subtle"),
    ("tool.family.unknown", "warning"),
    ("license_badge.neutral.bg", "bg_panel"),
    ("license_badge.neutral.fg", "fg_subtle"),
    ("license_badge.success.bg", "success"),
    ("license_badge.success.fg", "fg_on_success"),
    ("license_badge.warning.bg", "warning"),
    ("license_badge.warning.fg", "fg_on_warning"),
    ("license_badge.danger.bg", "danger"),
    ("license_badge.danger.fg", "fg_on_danger"),
    ("plan.stage.queued", "fg_muted"),
    ("plan.stage.running", "info"),
    ("plan.stage.done", "success"),
    ("plan.stage.failed", "danger"),
    ("plan.stage.blocked", "warning"),
    ("diff.add.fg", "diff_add"),
    ("diff.del.fg", "diff_del"),
    ("diff.context.fg", "fg_muted"),
    ("activity.think", "fg_muted"),
    ("activity.act", "accent_alt"),
    ("activity.observe", "success"),
    ("activity.delegate", "accent"),
    ("activity.complete", "success"),
    ("activity.error", "danger"),
    ("activity.user_message", "accent"),
    ("activity.permission", "warning"),
    ("activity.info", "fg"),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenMap(pub HashMap<String, String>);

impl TokenMap {
    pub fn dark_default() -> TokenMap {
        TokenMap::from_bindings(DARK_DEFAULT_BINDINGS)
    }

    pub fn light_default() -> TokenMap {
        Self::dark_default()
    }

    pub fn high_contrast_default() -> TokenMap {
        Self::dark_default()
    }

    pub fn resolve<'a>(&self, token: &str, palette: &'a Palette) -> Option<&'a PaletteEntry> {
        let palette_field = self.0.get(token)?;
        palette_entry(palette_field, palette)
    }

    fn from_bindings(bindings: &[(&str, &str)]) -> TokenMap {
        let mut token_map = HashMap::with_capacity(bindings.len());
        for &(token, palette_field) in bindings {
            token_map.insert(token.to_string(), palette_field.to_string());
        }
        TokenMap(token_map)
    }
}

fn palette_entry<'a>(field: &str, palette: &'a Palette) -> Option<&'a PaletteEntry> {
    match field {
        "bg" => Some(&palette.bg),
        "bg_panel" => Some(&palette.bg_panel),
        "bg_selection" => Some(&palette.bg_selection),
        "bg_overlay" => Some(&palette.bg_overlay),
        "fg" => Some(&palette.fg),
        "fg_muted" => Some(&palette.fg_muted),
        "fg_subtle" => Some(&palette.fg_subtle),
        "fg_on_accent" => Some(&palette.fg_on_accent),
        "fg_on_success" => Some(&palette.fg_on_success),
        "fg_on_warning" => Some(&palette.fg_on_warning),
        "fg_on_danger" => Some(&palette.fg_on_danger),
        "fg_on_info" => Some(&palette.fg_on_info),
        "fg_on_overlay" => Some(&palette.fg_on_overlay),
        "border" => Some(&palette.border),
        "border_focused" => Some(&palette.border_focused),
        "accent" => Some(&palette.accent),
        "accent_alt" => Some(&palette.accent_alt),
        "success" => Some(&palette.success),
        "warning" => Some(&palette.warning),
        "danger" => Some(&palette.danger),
        "info" => Some(&palette.info),
        "highlight" => Some(&palette.highlight),
        "diff_add" => Some(&palette.diff_add),
        "diff_del" => Some(&palette.diff_del),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::palette::Palette;
    use super::TokenMap;

    #[test]
    fn dark_default_contains_all_spec_sample_bindings() {
        assert_eq!(TokenMap::dark_default().0.len(), 49);
    }

    #[test]
    fn default_tokens_resolve_to_palette_entries() {
        let defaults = [
            (TokenMap::dark_default(), Palette::dark_default()),
            (TokenMap::light_default(), Palette::light_default()),
            (
                TokenMap::high_contrast_default(),
                Palette::high_contrast_default(),
            ),
        ];

        for (tokens, palette) in defaults {
            for token in tokens.0.keys() {
                assert!(tokens.resolve(token, &palette).is_some(), "{token}");
            }
        }
    }

    #[test]
    fn light_and_high_contrast_defaults_match_dark_bindings() {
        assert_eq!(TokenMap::light_default().0, TokenMap::dark_default().0);
        assert_eq!(
            TokenMap::high_contrast_default().0,
            TokenMap::dark_default().0
        );
    }

    #[test]
    fn unknown_token_returns_none() {
        let palette = Palette::dark_default();
        let tokens = TokenMap::dark_default();

        assert!(tokens.resolve("not.a.real.token", &palette).is_none());
    }

    #[test]
    fn migration_bindings_preserve_current_literal_colors() {
        let tokens = TokenMap::dark_default();

        assert_eq!(
            tokens.0.get("tool.family.edit"),
            Some(&"warning".to_string())
        );
        assert_eq!(
            tokens.0.get("tool.family.bash"),
            Some(&"accent_alt".to_string())
        );
        assert_eq!(
            tokens.0.get("status_bar.separator.fg"),
            Some(&"fg_subtle".to_string())
        );
    }
}
