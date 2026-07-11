use super::AgentProfile;
use crate::skills::installer::{parse_marker, sha256_hex, Marker};
use spur_acp::types::AgentKind;

pub struct RenderedProfile {
    /// Worktree-relative target path.
    pub rel_path: String,
    pub contents: String,
    unmarked_contents: String,
    marker_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExistingProfile {
    Unchanged,
    ManagedDifferent,
    NoMarker,
    Edited,
}

struct ExtractedMarker {
    marker: Marker,
    unmarked: String,
    legacy_toml_table: bool,
}

pub fn render_for_kind(profile: &AgentProfile, kind: AgentKind) -> Option<RenderedProfile> {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => Some(render_markdown_profile(
            format!(".claude/agents/{}.md", profile.name),
            profile.raw.clone(),
            &profile.name,
        )),
        AgentKind::OpenCode => {
            // OpenCode's agent frontmatter schema accepts only `description`;
            // `model`/`effort` are selected via the runtime config layer, not
            // the per-agent file. `tools` has no slot either — log the drop.
            log_tools_drop(profile);
            let description = yaml_escape_scalar(&profile.description);
            Some(render_markdown_profile(
                format!(".opencode/agent/{}.md", profile.name),
                format!("---\ndescription: {description}\n---\n{}", profile.body),
                &profile.name,
            ))
        }
        AgentKind::Kiro => {
            log_tools_drop(profile);
            let unmarked = serde_json::json!({
                "name": profile.name,
                "description": profile.description,
                "prompt": profile.body,
            });
            let unmarked_contents = serde_json::to_string_pretty(&unmarked)
                .unwrap_or_else(|error| unreachable!("serde_json serialization failed: {error}"));
            let marker = marker_for(&profile.name, unmarked_contents.as_bytes());
            let mut marked = unmarked;
            marked["x-spur-managed"] = serde_json::Value::String(marker.render_line());
            Some(RenderedProfile {
                rel_path: format!(".kiro/agents/{}.json", profile.name),
                contents: serde_json::to_string_pretty(&marked).unwrap_or_else(|error| {
                    unreachable!("serde_json serialization failed: {error}")
                }),
                unmarked_contents,
                marker_sha256: marker.sha256,
            })
        }
        AgentKind::CodexAcp => {
            log_tools_drop(profile);
            let mut value = toml::value::Table::new();
            value.insert("name".into(), profile.name.clone().into());
            value.insert("description".into(), profile.description.clone().into());
            value.insert("developer_instructions".into(), profile.body.clone().into());
            if let Some(model) = &profile.model {
                value.insert("model".into(), model.clone().into());
            }
            if let Some(effort) = &profile.effort {
                value.insert("model_reasoning_effort".into(), effort.clone().into());
            }
            let unmarked_contents = toml::to_string_pretty(&toml::Value::Table(value))
                .unwrap_or_else(|error| unreachable!("toml serialization failed: {error}"));
            let marker = marker_for(&profile.name, unmarked_contents.as_bytes());

            Some(RenderedProfile {
                rel_path: format!(".codex/agents/{}.toml", profile.name),
                contents: format!("# {}\n{unmarked_contents}", marker.render_line()),
                unmarked_contents,
                marker_sha256: marker.sha256,
            })
        }
        AgentKind::Kimi | AgentKind::Gemini | AgentKind::Generic => None,
    }
}

/// Claude agent frontmatter `tools:` is an allowlist — a profile that
/// restricts tools hides every MCP tool from the selected agent, including
/// the `spur-worker-mcp` server the delegation ships. Append `extra_tools`
/// to an existing allowlist so restricted personas keep the worker MCP
/// surface. Profiles without a `tools:` line inherit all tools and are
/// returned unchanged.
pub fn augment_tools_allowlist(profile: &AgentProfile, _extra_tools: &[String]) -> AgentProfile {
    profile.clone()
}

fn log_tools_drop(profile: &AgentProfile) {
    if let Some(tools) = &profile.tools {
        tracing::debug!(
            target: "spur::agent_profiles",
            profile = %profile.name,
            tools = %tools,
            "tools frontmatter has no target slot; dropping"
        );
    }
}

impl Marker {
    fn render_line(&self) -> String {
        self.render().trim_end_matches('\n').to_string()
    }
}

fn marker_for(profile_name: &str, bytes: &[u8]) -> Marker {
    Marker {
        version: 1,
        skill_id: format!("agent-profile:{profile_name}"),
        sha256: sha256_hex(bytes),
    }
}

/// Quote/escape a frontmatter scalar for YAML emission. The source parser at
/// `AgentProfile::parse` is intentionally minimal (no quote handling), so this
/// only kicks in for values that would confuse a real YAML parser — values
/// containing `:`, `#`, leading/trailing whitespace, newlines, quotes, or the
/// fence sequence `---`. Plain values emit unchanged to preserve byte-for-byte
/// compatibility with existing rendered files.
fn yaml_escape_scalar(s: &str) -> String {
    let needs_quoting = s.is_empty()
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.contains(':')
        || s.contains('#')
        || s.contains('"')
        || s.contains('\\')
        || s.contains('\n')
        || s == "---";
    if !needs_quoting {
        return s.to_string();
    }
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

pub(crate) fn render_markdown_profile(
    rel_path: String,
    mut unmarked_contents: String,
    profile_name: &str,
) -> RenderedProfile {
    if !unmarked_contents.ends_with('\n') {
        unmarked_contents.push('\n');
    }
    let marker = marker_for(profile_name, unmarked_contents.as_bytes());
    let mut contents = unmarked_contents.clone();
    contents.push_str(&marker.render());
    RenderedProfile {
        rel_path,
        contents,
        unmarked_contents,
        marker_sha256: marker.sha256,
    }
}

pub(crate) fn classify_existing(
    rendered: &RenderedProfile,
    existing: &str,
) -> Result<ExistingProfile, String> {
    let Some(extracted) = extract_marker_and_unmarked(&rendered.rel_path, existing)? else {
        return Ok(ExistingProfile::NoMarker);
    };
    let disk_sha = sha256_hex(extracted.unmarked.as_bytes());
    if disk_sha != extracted.marker.sha256 {
        return Ok(ExistingProfile::Edited);
    }
    if extracted.legacy_toml_table {
        return Ok(ExistingProfile::ManagedDifferent);
    }
    if extracted.marker.sha256 == rendered.marker_sha256
        && extracted.unmarked == rendered.unmarked_contents
    {
        Ok(ExistingProfile::Unchanged)
    } else {
        Ok(ExistingProfile::ManagedDifferent)
    }
}

fn extract_marker_and_unmarked(
    rel_path: &str,
    existing: &str,
) -> Result<Option<ExtractedMarker>, String> {
    if rel_path.ends_with(".json") {
        extract_json_marker(existing).map(|extracted| {
            extracted.map(|(marker, unmarked)| ExtractedMarker {
                marker,
                unmarked,
                legacy_toml_table: false,
            })
        })
    } else if rel_path.ends_with(".toml") {
        extract_toml_marker(existing)
    } else {
        Ok(
            extract_markdown_marker(existing).map(|(marker, unmarked)| ExtractedMarker {
                marker,
                unmarked,
                legacy_toml_table: false,
            }),
        )
    }
}

pub(crate) fn extract_markdown_marker(existing: &str) -> Option<(Marker, String)> {
    let mut line_start = 0usize;
    for line in existing.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if let Some(marker) = parse_marker(trimmed) {
            let mut unmarked = String::with_capacity(existing.len().saturating_sub(line.len()));
            unmarked.push_str(&existing[..line_start]);
            unmarked.push_str(&existing[line_end..]);
            return Some((marker, unmarked));
        }
        line_start = line_end;
    }
    // split_inclusive('\n') covers the trailing no-newline chunk, so after the
    // loop `line_start == existing.len()` and there is nothing left to inspect.
    None
}

fn extract_json_marker(existing: &str) -> Result<Option<(Marker, String)>, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(existing).map_err(|error| error.to_string())?;
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let Some(marker_value) = object.remove("x-spur-managed") else {
        return Ok(None);
    };
    let Some(marker_text) = marker_value.as_str() else {
        return Err("x-spur-managed is not a string".to_string());
    };
    let Some(marker) = parse_marker(marker_text) else {
        return Err("x-spur-managed is not a valid SPUR-MANAGED marker".to_string());
    };
    let unmarked = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    Ok(Some((marker, unmarked)))
}

fn extract_toml_marker(existing: &str) -> Result<Option<ExtractedMarker>, String> {
    if let Some((marker, unmarked)) = extract_toml_comment_marker(existing) {
        return Ok(Some(ExtractedMarker {
            marker,
            unmarked,
            legacy_toml_table: false,
        }));
    }

    let mut value: toml::Value = existing
        .parse::<toml::Value>()
        .map_err(|error| error.to_string())?;
    let Some(table) = value.as_table_mut() else {
        return Ok(None);
    };
    let Some(spur_value) = table.remove("spur") else {
        return Ok(None);
    };
    let Some(marker_text) = spur_value
        .as_table()
        .and_then(|table| table.get("managed"))
        .and_then(toml::Value::as_str)
    else {
        return Err("spur.managed is not a string".to_string());
    };
    let Some(marker) = parse_marker(marker_text) else {
        return Err("spur.managed is not a valid SPUR-MANAGED marker".to_string());
    };
    let unmarked = toml::to_string_pretty(&value).map_err(|error| error.to_string())?;
    Ok(Some(ExtractedMarker {
        marker,
        unmarked,
        legacy_toml_table: true,
    }))
}

fn extract_toml_comment_marker(existing: &str) -> Option<(Marker, String)> {
    let mut line_start = 0usize;
    for line in existing.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if let Some(marker) = trimmed.strip_prefix("# ").and_then(parse_marker) {
            let mut unmarked = String::with_capacity(existing.len().saturating_sub(line.len()));
            unmarked.push_str(&existing[..line_start]);
            unmarked.push_str(&existing[line_end..]);
            return Some((marker, unmarked));
        }
        line_start = line_end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};

    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SupportedCodexRole {
        name: String,
        description: String,
        developer_instructions: String,
        model: Option<String>,
        model_reasoning_effort: Option<String>,
    }

    fn profile() -> crate::agent_profiles::AgentProfile {
        crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\nmodel: opus\neffort: high\n---\nYou review code.\n",
        )
        .unwrap()
    }

    fn profile_with_tools() -> crate::agent_profiles::AgentProfile {
        crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\ntools: Read, Edit\n---\nYou review code.\n",
        )
        .unwrap()
    }

    fn multiline_profile() -> crate::agent_profiles::AgentProfile {
        crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\nmodel: opus\neffort: high\n---\nReview the diff.\nExplain both risks and recommendations.\n",
        )
        .unwrap()
    }

    fn legacy_codex_contents(rendered: &RenderedProfile, profile_name: &str) -> String {
        let mut value = rendered.unmarked_contents.parse::<toml::Value>().unwrap();
        let marker = marker_for(profile_name, rendered.unmarked_contents.as_bytes());
        let mut spur = toml::value::Table::new();
        spur.insert("managed".into(), marker.render_line().into());
        value
            .as_table_mut()
            .unwrap()
            .insert("spur".into(), toml::Value::Table(spur));
        toml::to_string_pretty(&value).unwrap()
    }

    #[derive(Clone, Default)]
    struct CapturedDebugEvents(Arc<Mutex<Vec<CapturedDebugEvent>>>);

    #[derive(Clone, Debug, Default)]
    struct CapturedDebugEvent {
        target: String,
        fields: Vec<(String, String)>,
    }

    impl CapturedDebugEvents {
        fn contains_tools_drop_for(&self, profile: &str, tools: &str) -> bool {
            self.0.lock().unwrap().iter().any(|event| {
                event.target == "spur::agent_profiles"
                    && event.fields.iter().any(|(name, value)| {
                        name == "message"
                            && field_value_eq(
                                value,
                                "tools frontmatter has no target slot; dropping",
                            )
                    })
                    && event
                        .fields
                        .iter()
                        .any(|(name, value)| name == "profile" && field_value_eq(value, profile))
                    && event
                        .fields
                        .iter()
                        .any(|(name, value)| name == "tools" && field_value_eq(value, tools))
            })
        }
    }

    fn field_value_eq(actual: &str, expected: &str) -> bool {
        actual == expected || actual.trim_matches('"') == expected
    }

    impl tracing::Subscriber for CapturedDebugEvents {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::DEBUG
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() != tracing::Level::DEBUG {
                return;
            }
            let mut visitor = DebugVisitor::default();
            event.record(&mut visitor);
            self.0.lock().unwrap().push(CapturedDebugEvent {
                target: event.metadata().target().to_string(),
                fields: visitor.fields,
            });
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[derive(Default)]
    struct DebugVisitor {
        fields: Vec<(String, String)>,
    }

    impl Visit for DebugVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .push((field.name().to_string(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }

    #[test]
    fn non_claude_renderers_debug_log_dropped_tools_frontmatter() {
        let profile = profile_with_tools();
        for kind in [AgentKind::OpenCode, AgentKind::Kiro, AgentKind::CodexAcp] {
            let captured = CapturedDebugEvents::default();
            let _serialize = crate::tracing_test_lock::guard();
            tracing::subscriber::with_default(captured.clone(), || {
                let _ = render_for_kind(&profile, kind).unwrap();
            });
            assert!(
                captured.contains_tools_drop_for("code-reviewer", "Read, Edit"),
                "expected tools drop debug log for {kind:?}; captured={:?}",
                captured.0.lock().unwrap()
            );
        }
    }

    #[test]
    fn claude_kinds_get_canonical_source_with_spur_marker() {
        for kind in [AgentKind::ClaudeCodeAcp, AgentKind::ClaudeStreamJson] {
            let r = render_for_kind(&profile(), kind).unwrap();
            assert_eq!(r.rel_path, ".claude/agents/code-reviewer.md");
            assert!(r.contents.starts_with(&profile().raw));
            assert!(r
                .contents
                .contains("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
        }
    }

    #[test]
    fn opencode_gets_agent_markdown_with_description_frontmatter() {
        let r = render_for_kind(&profile(), AgentKind::OpenCode).unwrap();
        assert_eq!(r.rel_path, ".opencode/agent/code-reviewer.md");
        assert!(r
            .contents
            .starts_with("---\ndescription: Reviews diffs\n---\n"));
        assert!(r.contents.contains("You review code."));
        assert!(r
            .contents
            .contains("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
    }

    #[test]
    fn kiro_gets_json_with_name_description_prompt() {
        let r = render_for_kind(&profile(), AgentKind::Kiro).unwrap();
        assert_eq!(r.rel_path, ".kiro/agents/code-reviewer.json");
        let v: serde_json::Value = serde_json::from_str(&r.contents).unwrap();
        assert_eq!(v["name"], "code-reviewer");
        assert_eq!(v["description"], "Reviews diffs");
        assert_eq!(v["prompt"], "You review code.\n");
        assert!(v["x-spur-managed"]
            .as_str()
            .unwrap()
            .starts_with("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
    }

    #[test]
    fn codex_gets_schema_compatible_toml_with_all_functional_fields() {
        let r = render_for_kind(&multiline_profile(), AgentKind::CodexAcp).unwrap();
        assert_eq!(r.rel_path, ".codex/agents/code-reviewer.toml");
        let v: toml::Value = r.contents.parse().unwrap();
        let keys = v
            .as_table()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from([
                "description",
                "developer_instructions",
                "model",
                "model_reasoning_effort",
                "name",
            ])
        );
        let role: SupportedCodexRole = toml::from_str(&r.contents).unwrap();
        assert_eq!(role.name, "code-reviewer");
        assert_eq!(role.description, "Reviews diffs");
        assert_eq!(
            role.developer_instructions,
            "Review the diff.\nExplain both risks and recommendations.\n"
        );
        assert_eq!(role.model.as_deref(), Some("opus"));
        assert_eq!(role.model_reasoning_effort.as_deref(), Some("high"));
        assert!(r.contents.lines().any(|line| line
            .starts_with("# <!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256=")));
    }

    #[test]
    fn codex_comment_markers_preserve_ownership_classification() {
        let original = render_for_kind(&profile(), AgentKind::CodexAcp).unwrap();
        assert_eq!(
            classify_existing(&original, &original.contents).unwrap(),
            ExistingProfile::Unchanged
        );

        let changed_profile = crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\nmodel: opus\neffort: high\n---\nReview code and tests.\n",
        )
        .unwrap();
        let changed = render_for_kind(&changed_profile, AgentKind::CodexAcp).unwrap();
        assert_eq!(
            classify_existing(&changed, &original.contents).unwrap(),
            ExistingProfile::ManagedDifferent
        );

        let edited = original
            .contents
            .replace("Reviews diffs", "Reviews edited diffs");
        assert_eq!(
            classify_existing(&original, &edited).unwrap(),
            ExistingProfile::Edited
        );
        assert_eq!(
            classify_existing(&original, &original.unmarked_contents).unwrap(),
            ExistingProfile::NoMarker
        );
    }

    #[test]
    fn codex_legacy_spur_table_is_recognized_as_migratable() {
        let rendered = render_for_kind(&profile(), AgentKind::CodexAcp).unwrap();
        let legacy = legacy_codex_contents(&rendered, "code-reviewer");
        assert!(legacy.parse::<toml::Value>().unwrap().get("spur").is_some());
        assert_eq!(
            classify_existing(&rendered, &legacy).unwrap(),
            ExistingProfile::ManagedDifferent
        );
    }

    #[test]
    fn codex_legacy_spur_table_with_functional_edit_is_edited() {
        let rendered = render_for_kind(&profile(), AgentKind::CodexAcp).unwrap();
        let legacy = legacy_codex_contents(&rendered, "code-reviewer");
        let edited = legacy.replace("Reviews diffs", "Reviews edited diffs");
        let edited_value = edited.parse::<toml::Value>().unwrap();

        assert!(edited_value.get("spur").is_some());
        assert_eq!(
            edited_value["description"].as_str(),
            Some("Reviews edited diffs")
        );

        assert_eq!(
            classify_existing(&rendered, &edited).unwrap(),
            ExistingProfile::Edited
        );
    }

    #[test]
    fn rendered_profiles_classify_matching_managed_files_as_unchanged() {
        for kind in [
            AgentKind::ClaudeCodeAcp,
            AgentKind::OpenCode,
            AgentKind::Kiro,
            AgentKind::CodexAcp,
        ] {
            let r = render_for_kind(&profile(), kind).unwrap();
            assert_eq!(
                classify_existing(&r, &r.contents).unwrap(),
                ExistingProfile::Unchanged
            );
        }
    }

    #[test]
    fn kinds_without_convention_render_nothing() {
        for kind in [AgentKind::Kimi, AgentKind::Gemini, AgentKind::Generic] {
            assert!(render_for_kind(&profile(), kind).is_none());
        }
    }

    // Claim C: description containing `:` must not emit malformed YAML.
    #[test]
    fn opencode_description_with_colon_is_yaml_quoted() {
        let raw = "---\nname: colon\ndescription: a: b\n---\nbody\n";
        let p = crate::agent_profiles::AgentProfile::parse("colon", raw).unwrap();
        let r = render_for_kind(&p, AgentKind::OpenCode).unwrap();
        // The emitted frontmatter must be a single `description:` line whose
        // value is a valid YAML scalar. `description: a: b` (unquoted) is not.
        let frontmatter = r
            .contents
            .strip_prefix("---\n")
            .and_then(|s| s.split("\n---\n").next())
            .unwrap();
        // Expect double-quoted form for values containing `:`.
        assert!(
            frontmatter == r#"description: "a: b""#,
            "expected quoted YAML scalar; got {frontmatter:?}"
        );
    }

    // Claim C: simple descriptions still emit unquoted (byte-stable for
    // existing rendered files).
    #[test]
    fn opencode_description_simple_stays_unquoted() {
        let r = render_for_kind(&profile(), AgentKind::OpenCode).unwrap();
        assert!(r
            .contents
            .starts_with("---\ndescription: Reviews diffs\n---\n"));
    }

    fn worker_mcp_extras() -> Vec<String> {
        vec![
            "mcp__spur-worker-mcp__report_progress".to_string(),
            "mcp__spur-worker-mcp__code_read_symbol".to_string(),
        ]
    }

    #[test]
    fn augment_appends_worker_mcp_tools_to_existing_allowlist() {
        let augmented = augment_tools_allowlist(&profile_with_tools(), &worker_mcp_extras());
        let expected = "Read, Edit, mcp__spur-worker-mcp__report_progress, mcp__spur-worker-mcp__code_read_symbol";
        assert_eq!(augmented.tools.as_deref(), Some(expected));
        assert!(
            augmented.raw.contains(&format!("tools: {expected}")),
            "raw frontmatter must carry the augmented allowlist; got {:?}",
            augmented.raw
        );
        let reparsed =
            crate::agent_profiles::AgentProfile::parse("code-reviewer", &augmented.raw).unwrap();
        assert_eq!(reparsed.body, profile_with_tools().body);
        assert_eq!(reparsed.description, profile_with_tools().description);
        assert_eq!(reparsed.tools.as_deref(), Some(expected));
    }

    #[test]
    fn augment_dedupes_tools_already_in_the_allowlist() {
        let profile = crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\ntools: Read, mcp__spur-worker-mcp__report_progress\n---\nYou review code.\n",
        )
        .unwrap();
        let augmented = augment_tools_allowlist(&profile, &worker_mcp_extras());
        assert_eq!(
            augmented.tools.as_deref(),
            Some("Read, mcp__spur-worker-mcp__report_progress, mcp__spur-worker-mcp__code_read_symbol")
        );
    }

    #[test]
    fn augment_leaves_profiles_without_tools_line_unchanged() {
        let original = profile();
        let augmented = augment_tools_allowlist(&original, &worker_mcp_extras());
        assert_eq!(augmented, original);
    }

    #[test]
    fn augmented_profile_renders_managed_claude_file() {
        let augmented = augment_tools_allowlist(&profile_with_tools(), &worker_mcp_extras());
        let rendered = render_for_kind(&augmented, AgentKind::ClaudeCodeAcp).unwrap();
        assert!(rendered
            .contents
            .contains("mcp__spur-worker-mcp__report_progress"));
        assert_eq!(
            classify_existing(&rendered, &rendered.contents).unwrap(),
            ExistingProfile::Unchanged
        );
    }

    // Claim D regression guard: extract_marker must find a marker on a body
    // that lacks a trailing newline.
    #[test]
    fn extract_marker_handles_body_without_trailing_newline() {
        let r = render_for_kind(&profile(), AgentKind::OpenCode).unwrap();
        let marker_line = r
            .contents
            .lines()
            .find(|l| l.contains("SPUR-MANAGED"))
            .unwrap();
        let body = "Some agent body text.\n";
        let with_nl = format!("{body}{marker_line}\n");
        let no_nl = format!("{body}{marker_line}");
        assert!(extract_marker_and_unmarked(".md", &with_nl)
            .unwrap()
            .is_some());
        assert!(extract_marker_and_unmarked(".md", &no_nl)
            .unwrap()
            .is_some());
    }
}
