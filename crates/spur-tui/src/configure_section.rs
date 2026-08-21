//! Section focus for `/configure` (`CONFIGURE-SECTION`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureSection {
    Agents,
    Graph,
    Tui,
    Skills,
}

impl ConfigureSection {
    pub const ALL: [Self; 4] = [Self::Agents, Self::Graph, Self::Tui, Self::Skills];

    pub fn parse_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "agents" | "agent" => Some(Self::Agents),
            "graph" => Some(Self::Graph),
            "tui" => Some(Self::Tui),
            "skills" | "skill" => Some(Self::Skills),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Graph => "graph",
            Self::Tui => "tui",
            Self::Skills => "skills",
        }
    }

    pub fn list_label(self) -> &'static str {
        match self {
            Self::Agents => "Agents",
            Self::Graph => "Graph",
            Self::Tui => "TUI",
            Self::Skills => "Skills",
        }
    }
}

/// Empty / omitted arg → agents. Reserved tokens → that section.
/// Any other token → agents + agent preselect (Phase 1).
pub fn parse_configure_arg(arg: &str) -> (ConfigureSection, Option<String>) {
    let trimmed = arg.trim();
    if trimmed.is_empty() {
        return (ConfigureSection::Agents, None);
    }
    if let Some(section) = ConfigureSection::parse_token(trimmed) {
        return (section, None);
    }
    (ConfigureSection::Agents, Some(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arg_focuses_agents() {
        assert_eq!(parse_configure_arg(""), (ConfigureSection::Agents, None));
        assert_eq!(parse_configure_arg("   "), (ConfigureSection::Agents, None));
    }

    #[test]
    fn reserved_tokens_focus_sections() {
        assert_eq!(
            parse_configure_arg("graph"),
            (ConfigureSection::Graph, None)
        );
        assert_eq!(parse_configure_arg("TUI"), (ConfigureSection::Tui, None));
        assert_eq!(
            parse_configure_arg("skills"),
            (ConfigureSection::Skills, None)
        );
        assert_eq!(
            parse_configure_arg("agents"),
            (ConfigureSection::Agents, None)
        );
    }

    #[test]
    fn unknown_token_is_agent_preselect() {
        assert_eq!(
            parse_configure_arg("kiro"),
            (ConfigureSection::Agents, Some("kiro".into()))
        );
    }

    #[test]
    fn list_labels_are_title_case() {
        assert_eq!(ConfigureSection::Agents.list_label(), "Agents");
        assert_eq!(ConfigureSection::Graph.list_label(), "Graph");
        assert_eq!(ConfigureSection::Tui.list_label(), "TUI");
        assert_eq!(ConfigureSection::Skills.list_label(), "Skills");
    }
}
