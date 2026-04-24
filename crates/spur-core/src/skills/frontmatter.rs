//! Parse `name` + `description` YAML frontmatter from a source SKILL.md,
//! and return the body with any leading SPUR-MANAGED marker stripped.
//!
//! Inputs come from two sources:
//! 1. Bundled SKILL.md files (have frontmatter, no marker).
//! 2. User-edited override files under `.spur/skills/<id>/SKILL.md`
//!    (have frontmatter AND a SPUR-MANAGED marker we wrote previously).
//!
//! Both cases flow through this parser.

use std::borrow::Cow;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedSource<'a> {
    pub name: Option<&'a str>,
    pub description: Option<Cow<'a, str>>,
    pub body: &'a str,
    pub role: Option<super::SkillRole>,
}

pub(crate) fn parse_source(raw: &str) -> ParsedSource<'_> {
    // Strip `---\n<yaml>\n---\n` frontmatter if present.
    let normalized = strip_leading_generated_comments(raw);
    let (frontmatter, after_fm) = match normalized.strip_prefix("---\n") {
        Some(rest) => match rest.find("\n---\n") {
            Some(idx) => (Some(&rest[..idx]), &rest[idx + 5..]),
            None => (None, normalized),
        },
        None => (None, normalized),
    };

    // Strip a leading SPUR-MANAGED marker line if present.
    let body = match after_fm.strip_prefix("<!-- SPUR-MANAGED ") {
        Some(rest) => match rest.find(" -->\n") {
            Some(idx) => &rest[idx + 5..],
            None => after_fm,
        },
        None => after_fm,
    };

    let mut name = None;
    let mut description = None;
    let mut role = None;
    if let Some(fm) = frontmatter {
        let lines: Vec<_> = fm.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if let Some(v) = line.strip_prefix("name:") {
                name = Some(v.trim());
            } else if let Some(v) = line.strip_prefix("description:") {
                description = Some(parse_yaml_scalar(v.trim(), &lines[idx + 1..]));
            } else if let Some(v) = line.strip_prefix("role:") {
                if let Ok(r) = v.trim().parse::<super::SkillRole>() {
                    role = Some(r);
                }
            }
        }
    }

    ParsedSource {
        name,
        description,
        body,
        role,
    }
}

fn strip_leading_generated_comments(mut raw: &str) -> &str {
    loop {
        let Some((line, rest)) = raw.split_once('\n') else {
            return raw;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || (trimmed.starts_with("<!--") && trimmed.ends_with("-->")) {
            raw = rest;
            continue;
        }
        return raw;
    }
}

fn parse_yaml_scalar<'a>(value: &'a str, following: &[&'a str]) -> Cow<'a, str> {
    if matches!(value, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
        let mut parts = Vec::new();
        for line in following {
            if line.trim().is_empty() {
                continue;
            }
            if !line.starts_with(' ') && !line.starts_with('\t') {
                break;
            }
            parts.push(line.trim());
        }
        return Cow::Owned(parts.join(" "));
    }

    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        return Cow::Owned(unescape_double_quoted(&value[1..value.len() - 1]));
    }

    Cow::Borrowed(value)
}

fn unescape_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_skill_with_frontmatter() {
        let raw = "---\nname: tdd\ndescription: Use for TDD\n---\nBody here\n";
        let p = parse_source(raw);
        assert_eq!(p.name, Some("tdd"));
        assert_eq!(p.description.as_deref(), Some("Use for TDD"));
        assert_eq!(p.body, "Body here\n");
        assert_eq!(p.role, None);
    }

    #[test]
    fn parses_role_frontmatter() {
        let raw = "---\nname: tdd\ndescription: Use for TDD\nrole: brain\n---\nBody\n";
        let p = parse_source(raw);
        assert_eq!(p.role, Some(super::super::SkillRole::Brain));
    }

    #[test]
    fn parses_worker_role() {
        let raw = "---\nname: debug\ndescription: Debug\nrole: worker\n---\nBody\n";
        let p = parse_source(raw);
        assert_eq!(p.role, Some(super::super::SkillRole::Worker));
    }

    #[test]
    fn ignores_unknown_role() {
        let raw = "---\nname: x\ndescription: X\nrole: alien\n---\nBody\n";
        let p = parse_source(raw);
        assert_eq!(p.role, None);
    }

    #[test]
    fn parses_override_with_marker() {
        let raw = "---\nname: tdd\ndescription: Override desc\n---\n\
                   <!-- SPUR-MANAGED v=1 skill=tdd sha256=abc -->\n\
                   Body after marker\n";
        let p = parse_source(raw);
        assert_eq!(p.name, Some("tdd"));
        assert_eq!(p.description.as_deref(), Some("Override desc"));
        assert_eq!(p.body, "Body after marker\n");
    }

    #[test]
    fn no_frontmatter_returns_raw_body() {
        let raw = "just body\n";
        let p = parse_source(raw);
        assert_eq!(p.name, None);
        assert_eq!(p.description, None);
        assert_eq!(p.body, "just body\n");
    }

    #[test]
    fn empty_description_is_empty_string() {
        let raw = "---\ndescription:\n---\nbody";
        let p = parse_source(raw);
        assert_eq!(p.description.as_deref(), Some(""));
    }

    #[test]
    fn parses_frontmatter_after_generated_comment() {
        let raw = "<!-- GENERATED BY SPUR. DO NOT EDIT. -->\n\n---\nname: tdd\ndescription: Use for TDD\n---\nBody\n";
        let p = parse_source(raw);
        assert_eq!(p.name, Some("tdd"));
        assert_eq!(p.description.as_deref(), Some("Use for TDD"));
        assert_eq!(p.body, "Body\n");
    }

    #[test]
    fn parses_folded_description() {
        let raw = "---\nname: tdd\ndescription: >\n  Use when writing tests.\n  Keeps implementation honest.\nrole: both\n---\nBody\n";
        let p = parse_source(raw);
        assert_eq!(
            p.description.as_deref(),
            Some("Use when writing tests. Keeps implementation honest.")
        );
        assert_eq!(p.role, Some(super::super::SkillRole::Both));
    }

    #[test]
    fn parses_escaped_double_quoted_description() {
        let raw = "---\nname: tdd\ndescription: \"Use when user says \\\"test\\\"\"\n---\nBody\n";
        let p = parse_source(raw);
        assert_eq!(
            p.description.as_deref(),
            Some("Use when user says \"test\"")
        );
    }
}
