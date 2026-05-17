use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PanicType {
    Bounds,
    Unwrap,
    OptionUnwrap,
    ResultUnwrap,
    Assertion,
    Other,
}

#[allow(dead_code)]
pub fn scrub_stack(raw: &str) -> String {
    raw.lines()
        .map(scrub_stack_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn scrub_stack_line(line: &str) -> String {
    let stripped = strip_home_prefixes(line);
    if let Some(tail) = extract_spur_symbol_tail(&stripped) {
        format!("<external>::{tail}")
    } else if let Some(idx) = find_spur_crate(&stripped) {
        let tail = &stripped[idx..];
        format!("<external>::{}", tail.trim_start_matches(':'))
    } else if looks_like_path_or_frame(&stripped) {
        "<external>".to_string()
    } else {
        stripped
    }
}

fn extract_spur_symbol_tail(s: &str) -> Option<&str> {
    for (idx, _) in s.match_indices("spur_") {
        let tail = &s[idx..];
        let Some(ns_pos) = tail.find("::") else {
            continue;
        };
        let head = &tail[..ns_pos];
        if head.contains('/') || head.contains('\\') {
            continue;
        }

        let end = tail
            .char_indices()
            .find_map(|(i, ch)| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == ':' {
                    None
                } else {
                    Some(i)
                }
            })
            .unwrap_or(tail.len());

        let symbol = &tail[..end];
        if symbol.contains("::") {
            return Some(symbol);
        }
    }

    None
}

fn find_spur_crate(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        if &bytes[i..i + 5] == b"spur_" {
            let prev_ok = i == 0 || !is_ident(bytes[i - 1]);
            if prev_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn looks_like_path_or_frame(s: &str) -> bool {
    s.contains("::") || s.contains('/') || s.contains('\\')
}

fn strip_home_prefixes(s: &str) -> String {
    let mut out = s.to_string();
    out = strip_prefix_family(&out, "/Users/", '/');
    out = strip_prefix_family(&out, "/home/", '/');
    out = strip_prefix_family(&out, "C:\\Users\\", '\\');
    out
}

fn strip_prefix_family(input: &str, prefix: &str, sep: char) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(pos) = rest.find(prefix) {
        out.push_str(&rest[..pos]);
        let after_prefix = &rest[pos + prefix.len()..];
        if let Some(user_end) = after_prefix.find(sep) {
            rest = &after_prefix[user_end + sep.len_utf8()..];
        } else {
            out.push_str(&rest[pos..]);
            return out;
        }
    }

    out.push_str(rest);
    out
}

#[allow(dead_code)]
pub fn bucket_model(name: &str) -> &'static str {
    let normalized = name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "claude-opus-4-7" => "claude-opus-4-7",
        "claude-opus-4-6" => "claude-opus-4-6",
        "claude-opus-4-5" => "claude-opus-4-5",
        "claude-sonnet-4-7" => "claude-sonnet-4-7",
        "claude-sonnet-4-6" => "claude-sonnet-4-6",
        "claude-sonnet-4-5" => "claude-sonnet-4-5",
        "claude-haiku-4-5" => "claude-haiku-4-5",
        "gpt-5" => "gpt-5",
        "gpt-5-codex" => "gpt-5-codex",
        "gpt-4o" => "gpt-4o",
        "gpt-4o-mini" => "gpt-4o-mini",
        "gemini-2.5-pro" => "gemini-2.5-pro",
        "gemini-2.5-flash" => "gemini-2.5-flash",
        _ if normalized.starts_with("claude") || normalized.starts_with("anthropic") => {
            "anthropic_other"
        }
        _ if normalized.starts_with("gpt")
            || normalized.starts_with("o1")
            || normalized.starts_with("o3")
            || normalized.starts_with("o4")
            || normalized.starts_with("openai") =>
        {
            "openai_other"
        }
        _ if normalized.starts_with("gemini") || normalized.starts_with("google") => "google_other",
        _ if normalized.starts_with("local") || normalized.starts_with("llama") => "local_other",
        _ => "other",
    }
}

#[allow(dead_code)]
pub fn classify_panic(msg: &str) -> PanicType {
    if msg.contains("index out of bounds") {
        return PanicType::Bounds;
    }
    if msg.contains("called `Option::unwrap()`") {
        return PanicType::OptionUnwrap;
    }
    if msg.contains("called `Result::unwrap()`") {
        return PanicType::ResultUnwrap;
    }
    if msg.contains("called `unwrap()`") {
        return PanicType::Unwrap;
    }
    if msg.contains("assertion failed") {
        return PanicType::Assertion;
    }
    PanicType::Other
}

#[allow(dead_code)]
pub fn payload_hash(msg: &str, anonymous_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(msg.as_bytes());
    hasher.update(anonymous_id.as_bytes());
    let digest = hasher.finalize();
    digest[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

pub fn sha256_prefix(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    digest[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

#[allow(dead_code)]
pub fn classify_server(server_name: &str) -> crate::tier2_events::McpServerName {
    match server_name.trim().to_ascii_lowercase().as_str() {
        "github" => crate::tier2_events::McpServerName::Github,
        "posthog" => crate::tier2_events::McpServerName::Posthog,
        "spur-mcp" => crate::tier2_events::McpServerName::SpurMcp,
        "stitch" => crate::tier2_events::McpServerName::Stitch,
        "playwright" => crate::tier2_events::McpServerName::Playwright,
        "context7" => crate::tier2_events::McpServerName::Context7,
        "firebase" => crate::tier2_events::McpServerName::Firebase,
        "sequential-thinking" => crate::tier2_events::McpServerName::SequentialThinking,
        _ => crate::tier2_events::McpServerName::Custom(
            crate::tier2_events::HashedShort::from_sha256_prefix(server_name),
        ),
    }
}

#[allow(dead_code)]
pub fn classify_tool(server_name: &str, tool_name: &str) -> crate::tier2_events::McpToolName {
    let server = classify_server(server_name);
    if matches!(server, crate::tier2_events::McpServerName::Custom(_)) {
        return crate::tier2_events::McpToolName::Custom(
            crate::tier2_events::HashedShort::from_sha256_prefix(tool_name),
        );
    }

    match tool_name.trim() {
        "submit_plan" => crate::tier2_events::McpToolName::SubmitPlan,
        "dispatch_task" => crate::tier2_events::McpToolName::DispatchTask,
        "review_task" => crate::tier2_events::McpToolName::ReviewTask,
        "get_task_diff" => crate::tier2_events::McpToolName::GetTaskDiff,
        "list_tools" => crate::tier2_events::McpToolName::ListTools,
        _ => crate::tier2_events::McpToolName::Custom(
            crate::tier2_events::HashedShort::from_sha256_prefix(tool_name),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_stack_cross_platform_fixtures() {
        let fixtures = [
            (
                r#"0: /Users/alice/work/spur/crates/spur_core/src/lib.rs:42:7 (spur_core::engine::run::42)
1: /Users/alice/.cargo/registry/src/foo.rs:11:2 (serde::de::impl::11)"#,
                "<external>::spur_core::engine::run::42\n<external>",
            ),
            (
                r#"0: /home/bob/spur/crates/spur_tui/src/app.rs:101:9 (spur_tui::app::tick::101)
1: /home/bob/.rustup/toolchains/stable/libstd/panicking.rs:200:1"#,
                "<external>::spur_tui::app::tick::101\n<external>",
            ),
            (
                r#"0: C:\\Users\\carol\\spur\\crates\\spur_acp\\src\\client.rs:77:3 (spur_acp::client::poll::77)
1: C:\\Users\\carol\\.cargo\\registry\\src\\bar.rs:4:1"#,
                "<external>::spur_acp::client::poll::77\n<external>",
            ),
        ];

        for (input, expected) in fixtures {
            assert_eq!(scrub_stack(input), expected);
        }
    }

    #[test]
    fn bucket_model_maps_known_and_unknown() {
        assert_eq!(bucket_model("claude-opus-4-7"), "claude-opus-4-7");
        assert_eq!(bucket_model("claude-foo-bar"), "anthropic_other");
        assert_eq!(bucket_model("xyz"), "other");
    }

    #[test]
    fn classify_panic_variants() {
        assert_eq!(
            classify_panic("thread panicked: index out of bounds: len is 1 but index is 2"),
            PanicType::Bounds
        );
        assert_eq!(
            classify_panic("thread panicked: called `unwrap()` on a `None` value"),
            PanicType::Unwrap
        );
        assert_eq!(
            classify_panic("thread panicked: called `Option::unwrap()` on a `None` value"),
            PanicType::OptionUnwrap
        );
        assert_eq!(
            classify_panic("thread panicked: called `Result::unwrap()` on an `Err` value"),
            PanicType::ResultUnwrap
        );
        assert_eq!(
            classify_panic("thread panicked: assertion failed: x > 0"),
            PanicType::Assertion
        );
        assert_eq!(classify_panic("something else"), PanicType::Other);
    }

    #[test]
    fn payload_hash_is_peppered_and_deterministic() {
        let a = payload_hash("boom", "anon-a");
        let b = payload_hash("boom", "anon-a");
        let c = payload_hash("boom", "anon-b");

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn sha256_prefix_is_deterministic() {
        let a = sha256_prefix("sample");
        let b = sha256_prefix("sample");
        let c = sha256_prefix("different");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn classify_server_uses_allowlist_or_hash() {
        let known = classify_server("github");
        let unknown = classify_server("internal-private-server");

        assert!(matches!(known, crate::tier2_events::McpServerName::Github));
        assert!(matches!(
            unknown,
            crate::tier2_events::McpServerName::Custom(_)
        ));
    }

    #[test]
    fn classify_tool_is_symmetric_with_server_policy() {
        let hashed_on_unknown_server = classify_tool("private-mcp", "submit_plan");
        let allowlisted_on_known_server = classify_tool("github", "submit_plan");

        assert!(matches!(
            hashed_on_unknown_server,
            crate::tier2_events::McpToolName::Custom(_)
        ));
        assert!(matches!(
            allowlisted_on_known_server,
            crate::tier2_events::McpToolName::SubmitPlan
        ));
    }
}
