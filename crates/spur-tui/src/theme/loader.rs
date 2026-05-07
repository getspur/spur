use super::palette::{Palette, PaletteEntry};
use super::tokens::TokenMap;
use ratatui::style::Color;
use serde::Deserialize;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTheme {
    pub version: Option<u32>,
    pub name: Option<String>,
    pub extends: Option<String>,
    #[serde(default)]
    pub palette: BTreeMap<String, RawPaletteEntry>,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum RawPaletteEntry {
    Rgb(String),
    Fields(RawPaletteFields),
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPaletteFields {
    pub rgb: Option<String>,
    pub ansi: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Theme {
    pub name: String,
    pub palette: Palette,
    pub tokens: TokenMap,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("unsupported theme version: expected 1, found {found:?}")]
    UnsupportedVersion { found: Option<u32> },
    #[error("theme `{child}` extends `{parent}`, but `{parent}` also extends `{grandparent}`")]
    ChainedExtends {
        child: String,
        parent: String,
        grandparent: String,
    },
    #[error("token `{token}` uses literal hex `{value}`; tokens must reference palette keys")]
    LiteralHexInTokens { token: String, value: String },
    #[error("palette key `{key}` has invalid RGB hex `{value}`")]
    InvalidHex { key: String, value: String },
    #[error("palette key `{key}` has invalid ANSI color `{value}`")]
    InvalidAnsi { key: String, value: String },
    #[error("parent theme `{name}` was not found")]
    UnknownParent { name: String },
    #[error("invalid theme YAML: {0}")]
    InvalidYaml(#[from] serde_yml::Error),
}

#[derive(Default)]
struct PalettePatch {
    rgb: Option<Color>,
    ansi: Option<Color>,
}

pub fn load_theme_from_str(
    yaml: &str,
    parent_resolver: impl Fn(&str) -> Option<RawTheme>,
) -> Result<Theme, ThemeError> {
    let raw = serde_yml::from_str(yaml)?;
    load_theme(raw, &parent_resolver)
}

fn load_theme(
    raw: RawTheme,
    parent_resolver: &impl Fn(&str) -> Option<RawTheme>,
) -> Result<Theme, ThemeError> {
    ensure_supported_version(&raw)?;

    let mut diagnostics = Vec::new();
    let (mut palette, mut tokens) = if let Some(parent_name) = raw.extends.as_deref() {
        let parent = parent_resolver(parent_name).ok_or_else(|| ThemeError::UnknownParent {
            name: parent_name.to_string(),
        })?;
        if let Some(grandparent) = parent.extends.as_deref() {
            return Err(ThemeError::ChainedExtends {
                child: raw.name.clone().unwrap_or_else(|| "unnamed".to_string()),
                parent: parent_name.to_string(),
                grandparent: grandparent.to_string(),
            });
        }
        ensure_supported_version(&parent)?;

        let parent_theme = materialize_theme(parent, Vec::new())?;
        diagnostics = parent_theme.diagnostics;
        (parent_theme.palette, parent_theme.tokens)
    } else {
        (Palette::dark_default(), TokenMap::dark_default())
    };

    apply_raw_theme(&raw, &mut palette, &mut tokens, &mut diagnostics)?;

    Ok(Theme {
        name: raw.name.unwrap_or_else(|| "unnamed".to_string()),
        palette,
        tokens,
        diagnostics,
    })
}

fn materialize_theme(raw: RawTheme, mut diagnostics: Vec<String>) -> Result<Theme, ThemeError> {
    ensure_supported_version(&raw)?;

    let mut palette = Palette::dark_default();
    let mut tokens = TokenMap::dark_default();
    apply_raw_theme(&raw, &mut palette, &mut tokens, &mut diagnostics)?;

    Ok(Theme {
        name: raw.name.unwrap_or_else(|| "unnamed".to_string()),
        palette,
        tokens,
        diagnostics,
    })
}

fn ensure_supported_version(raw: &RawTheme) -> Result<(), ThemeError> {
    match raw.version {
        Some(1) => Ok(()),
        found => Err(ThemeError::UnsupportedVersion { found }),
    }
}

fn apply_raw_theme(
    raw: &RawTheme,
    palette: &mut Palette,
    tokens: &mut TokenMap,
    diagnostics: &mut Vec<String>,
) -> Result<(), ThemeError> {
    for (key, raw_entry) in &raw.palette {
        let patch = parse_palette_patch(key, raw_entry)?;
        let Some(entry) = palette_entry_mut(palette, key) else {
            diagnostics.push(format!("unknown palette key `{key}`"));
            continue;
        };

        if let Some(rgb) = patch.rgb {
            entry.rgb = rgb;
        }
        if let Some(ansi) = patch.ansi {
            entry.ansi = Some(ansi);
        }
    }

    let canonical_tokens = TokenMap::dark_default();
    for (token, value) in &raw.tokens {
        if looks_like_hex_literal(value) {
            return Err(ThemeError::LiteralHexInTokens {
                token: token.clone(),
                value: value.clone(),
            });
        }
        if !canonical_tokens.0.contains_key(token) {
            diagnostics.push(format!("unknown token key `{token}`"));
        }
        tokens.0.insert(token.clone(), value.clone());
    }

    Ok(())
}

fn parse_palette_patch(key: &str, raw_entry: &RawPaletteEntry) -> Result<PalettePatch, ThemeError> {
    match raw_entry {
        RawPaletteEntry::Rgb(rgb) => Ok(PalettePatch {
            rgb: Some(parse_rgb_hex(key, rgb)?),
            ansi: None,
        }),
        RawPaletteEntry::Fields(fields) => {
            let rgb = fields
                .rgb
                .as_deref()
                .map(|rgb| parse_rgb_hex(key, rgb))
                .transpose()?;
            let ansi = fields
                .ansi
                .as_deref()
                .map(|ansi| parse_ansi(key, ansi))
                .transpose()?;
            Ok(PalettePatch { rgb, ansi })
        }
    }
}

fn parse_rgb_hex(key: &str, value: &str) -> Result<Color, ThemeError> {
    let Some(hex) = value.strip_prefix('#') else {
        return Err(invalid_hex(key, value));
    };

    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid_hex(key, value));
    }

    match hex.len() {
        3 => {
            let mut nibbles = hex.chars().filter_map(|c| c.to_digit(16).map(|d| d as u8));
            let r = nibbles.next().expect("validated hex digit") * 17;
            let g = nibbles.next().expect("validated hex digit") * 17;
            let b = nibbles.next().expect("validated hex digit") * 17;
            Ok(Color::Rgb(r, g, b))
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| invalid_hex(key, value))?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| invalid_hex(key, value))?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| invalid_hex(key, value))?;
            Ok(Color::Rgb(r, g, b))
        }
        _ => Err(invalid_hex(key, value)),
    }
}

fn invalid_hex(key: &str, value: &str) -> ThemeError {
    ThemeError::InvalidHex {
        key: key.to_string(),
        value: value.to_string(),
    }
}

fn parse_ansi(key: &str, value: &str) -> Result<Color, ThemeError> {
    let normalized: String = value
        .chars()
        .filter(|c| *c != '-' && *c != '_' && !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect();

    match normalized.as_str() {
        "black" => Ok(Color::Black),
        "red" => Ok(Color::Red),
        "green" => Ok(Color::Green),
        "yellow" => Ok(Color::Yellow),
        "blue" => Ok(Color::Blue),
        "magenta" => Ok(Color::Magenta),
        "cyan" => Ok(Color::Cyan),
        "gray" | "grey" => Ok(Color::Gray),
        "darkgray" | "darkgrey" => Ok(Color::DarkGray),
        "lightred" => Ok(Color::LightRed),
        "lightgreen" => Ok(Color::LightGreen),
        "lightyellow" => Ok(Color::LightYellow),
        "lightblue" => Ok(Color::LightBlue),
        "lightmagenta" => Ok(Color::LightMagenta),
        "lightcyan" => Ok(Color::LightCyan),
        "white" => Ok(Color::White),
        _ => Err(ThemeError::InvalidAnsi {
            key: key.to_string(),
            value: value.to_string(),
        }),
    }
}

fn looks_like_hex_literal(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    (3..=8).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn palette_entry_mut<'a>(palette: &'a mut Palette, key: &str) -> Option<&'a mut PaletteEntry> {
    match key {
        "bg" => Some(&mut palette.bg),
        "bg_panel" => Some(&mut palette.bg_panel),
        "bg_selection" => Some(&mut palette.bg_selection),
        "bg_overlay" => Some(&mut palette.bg_overlay),
        "fg" => Some(&mut palette.fg),
        "fg_muted" => Some(&mut palette.fg_muted),
        "fg_subtle" => Some(&mut palette.fg_subtle),
        "fg_on_accent" => Some(&mut palette.fg_on_accent),
        "fg_on_success" => Some(&mut palette.fg_on_success),
        "fg_on_warning" => Some(&mut palette.fg_on_warning),
        "fg_on_danger" => Some(&mut palette.fg_on_danger),
        "fg_on_info" => Some(&mut palette.fg_on_info),
        "fg_on_overlay" => Some(&mut palette.fg_on_overlay),
        "border" => Some(&mut palette.border),
        "border_focused" => Some(&mut palette.border_focused),
        "accent" => Some(&mut palette.accent),
        "accent_alt" => Some(&mut palette.accent_alt),
        "success" => Some(&mut palette.success),
        "warning" => Some(&mut palette.warning),
        "danger" => Some(&mut palette.danger),
        "info" => Some(&mut palette.info),
        "highlight" => Some(&mut palette.highlight),
        "diff_add" => Some(&mut palette.diff_add),
        "diff_del" => Some(&mut palette.diff_del),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{load_theme_from_str, RawTheme, ThemeError};
    use ratatui::style::Color;

    fn load_without_parent(yaml: &str) -> Result<super::Theme, ThemeError> {
        load_theme_from_str(yaml, |_| None)
    }

    fn raw_theme(yaml: &str) -> RawTheme {
        serde_yml::from_str(yaml).expect("raw theme parses")
    }

    #[test]
    fn rejects_invalid_yaml() {
        let err = load_without_parent("version: [").expect_err("invalid yaml is rejected");

        assert!(matches!(err, ThemeError::InvalidYaml(_)));
    }

    #[test]
    fn rejects_missing_version() {
        let err = load_without_parent("name: missing-version\n")
            .expect_err("missing version is rejected");

        assert!(matches!(
            err,
            ThemeError::UnsupportedVersion { found: None }
        ));
    }

    #[test]
    fn rejects_unsupported_version() {
        let err = load_without_parent("version: 2\nname: bad-version\n")
            .expect_err("unsupported version is rejected");

        assert!(matches!(
            err,
            ThemeError::UnsupportedVersion { found: Some(2) }
        ));
    }

    #[test]
    fn rejects_unknown_parent_theme() {
        let err = load_without_parent("version: 1\nname: child\nextends: missing\n")
            .expect_err("unknown parent is rejected");

        assert!(matches!(
            err,
            ThemeError::UnknownParent { ref name } if name == "missing"
        ));
    }

    #[test]
    fn rejects_chained_extends() {
        let parent = raw_theme(
            r##"
version: 1
name: parent
extends: dark
"##,
        );

        let err = load_theme_from_str(
            r##"
version: 1
name: child
extends: parent
"##,
            |name| (name == "parent").then(|| parent.clone()),
        )
        .expect_err("chained extends is rejected");

        assert!(matches!(
            err,
            ThemeError::ChainedExtends {
                ref child,
                ref parent,
                ref grandparent,
            } if child == "child" && parent == "parent" && grandparent == "dark"
        ));
    }

    #[test]
    fn rejects_literal_hex_in_tokens() {
        let err = load_without_parent(
            r##"
version: 1
name: token-hex
tokens:
  picker.match.fg: "#ff79c6"
"##,
        )
        .expect_err("literal hex in token binding is rejected");

        assert!(matches!(
            err,
            ThemeError::LiteralHexInTokens {
                ref token,
                ref value,
            } if token == "picker.match.fg" && value == "#ff79c6"
        ));
    }

    #[test]
    fn ignores_token_values_that_do_not_match_hex_literal_regex() {
        for value in ["#gggg", "#123456789", "#12", "#RUST"] {
            let yaml = format!(
                r##"
version: 1
name: token-boundary
tokens:
  picker.match.fg: "{value}"
"##
            );

            load_without_parent(&yaml).expect("non-matching token values are allowed");
        }
    }

    #[test]
    fn rejects_malformed_palette_hex() {
        let err = load_without_parent(
            r##"
version: 1
name: bad-hex
palette:
  accent: "#12"
"##,
        )
        .expect_err("malformed palette hex is rejected");

        assert!(matches!(
            err,
            ThemeError::InvalidHex {
                ref key,
                ref value,
            } if key == "accent" && value == "#12"
        ));
    }

    #[test]
    fn rejects_unknown_ansi_name() {
        let err = load_without_parent(
            r##"
version: 1
name: bad-ansi
palette:
  accent:
    rgb: "#123456"
    ansi: ultraviolet
"##,
        )
        .expect_err("unknown ansi name is rejected");

        assert!(matches!(
            err,
            ThemeError::InvalidAnsi {
                ref key,
                ref value,
            } if key == "accent" && value == "ultraviolet"
        ));
    }

    #[test]
    fn warns_on_unknown_palette_key() {
        let theme = load_without_parent(
            r##"
version: 1
name: unknown-palette
palette:
  imaginary: "#123456"
"##,
        )
        .expect("unknown palette keys are warnings");

        assert_eq!(theme.diagnostics, ["unknown palette key `imaginary`"]);
    }

    #[test]
    fn warns_on_unknown_token_key() {
        let theme = load_without_parent(
            r##"
version: 1
name: unknown-token
tokens:
  future.token: accent
"##,
        )
        .expect("unknown token keys are warnings");

        assert_eq!(theme.diagnostics, ["unknown token key `future.token`"]);
        assert_eq!(
            theme.tokens.0.get("future.token"),
            Some(&"accent".to_string())
        );
    }

    #[test]
    fn rejects_unknown_top_level_fields() {
        let err = load_without_parent(
            r##"
version: 1
name: typo
palettes:
  accent: "#123456"
"##,
        )
        .expect_err("unknown top-level fields are rejected");

        assert!(matches!(err, ThemeError::InvalidYaml(_)));
    }

    #[test]
    fn rejects_unknown_palette_entry_fields() {
        let err = load_without_parent(
            r##"
version: 1
name: typo
palette:
  accent:
    rgd: "#ffffff"
"##,
        )
        .expect_err("unknown palette entry fields are rejected");

        assert!(matches!(err, ThemeError::InvalidYaml(_)));
    }

    #[test]
    fn child_rgb_override_preserves_parent_ansi() {
        let parent = raw_theme(
            r##"
version: 1
name: parent
palette:
  accent:
    rgb: "#000000"
    ansi: magenta
"##,
        );

        let theme = load_theme_from_str(
            r##"
version: 1
name: child
extends: parent
palette:
  accent: "#123456"
"##,
            |name| (name == "parent").then(|| parent.clone()),
        )
        .expect("child theme loads");

        assert_eq!(theme.palette.accent.rgb, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(theme.palette.accent.ansi, Some(Color::Magenta));
    }

    #[test]
    fn child_ansi_override_preserves_parent_rgb() {
        let parent = raw_theme(
            r##"
version: 1
name: parent
palette:
  accent:
    rgb: "#000000"
    ansi: magenta
"##,
        );

        let theme = load_theme_from_str(
            r##"
version: 1
name: child
extends: parent
palette:
  accent:
    ansi: blue
"##,
            |name| (name == "parent").then(|| parent.clone()),
        )
        .expect("child theme loads");

        assert_eq!(theme.palette.accent.rgb, Color::Rgb(0, 0, 0));
        assert_eq!(theme.palette.accent.ansi, Some(Color::Blue));
    }
}
