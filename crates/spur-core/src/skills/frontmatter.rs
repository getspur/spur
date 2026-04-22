//! Parse `name` + `description` YAML frontmatter from a source SKILL.md,
//! and return the body with any leading SPUR-MANAGED marker stripped.
//!
//! Inputs come from two sources:
//! 1. Bundled SKILL.md files (have frontmatter, no marker).
//! 2. User-edited override files under `.spur/skills/<id>/SKILL.md`
//!    (have frontmatter AND a SPUR-MANAGED marker we wrote previously).
//!
//! Both cases flow through this parser.

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedSource<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub body: &'a str,
    pub role: Option<super::SkillRole>,
}

pub(crate) fn parse_source(raw: &str) -> ParsedSource<'_> {
    // Strip `---\n<yaml>\n---\n` frontmatter if present.
    let (frontmatter, after_fm) = match raw.strip_prefix("---\n") {
        Some(rest) => match rest.find("\n---\n") {
            Some(idx) => (Some(&rest[..idx]), &rest[idx + 5..]),
            None => (None, raw),
        },
        None => (None, raw),
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
        for line in fm.lines() {
            if let Some(v) = line.strip_prefix("name:") {
                name = Some(v.trim());
            } else if let Some(v) = line.strip_prefix("description:") {
                description = Some(v.trim());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_skill_with_frontmatter() {
        let raw = "---\nname: tdd\ndescription: Use for TDD\n---\nBody here\n";
        let p = parse_source(raw);
        assert_eq!(p.name, Some("tdd"));
        assert_eq!(p.description, Some("Use for TDD"));
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
        assert_eq!(p.description, Some("Override desc"));
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
        assert_eq!(p.description, Some(""));
    }
}
