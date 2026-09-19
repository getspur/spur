//! Compatibility for observed Grok ACP request shapes.

use super::NormalizedTerminalCommand;

/// Normalize the packed argv shape emitted by Grok Build's ACP client.
///
/// This intentionally accepts only a POSIX-lexed `/bin/bash -lc|-c <script>`
/// command with exactly one script word. Every other request retains ACP's
/// protocol-correct direct-exec behavior. On non-Unix targets the shim is a
/// no-op.
///
/// Temporary Grok interop: remove this helper and its adapter dispatch once Grok
/// emits `command = "/bin/bash"` and `args = ["-lc", script]`.
pub(super) fn normalize_terminal_command(
    command: &str,
    args: &[String],
) -> Option<NormalizedTerminalCommand> {
    #[cfg(not(unix))]
    {
        let _ = (command, args);
        None
    }

    #[cfg(unix)]
    {
        if !args.is_empty() {
            return None;
        }

        let suffix = command.strip_prefix("/bin/bash")?;
        if !matches!(
            suffix.as_bytes().first(),
            Some(b' ' | b'\t' | b'\n' | b'\r')
        ) {
            return None;
        }

        let words = shell_words::split(command).ok()?;
        let [program, shell_flag, script] = words.as_slice() else {
            return None;
        };
        if program != "/bin/bash" || !matches!(shell_flag.as_str(), "-lc" | "-c") {
            return None;
        }

        tracing::info!(
            agent_kind = "grok",
            original_command_len = command.len(),
            shell_flag = %shell_flag,
            script_len = script.len(),
            "Grok adapter: applied temporary terminal/create argv interop; remove when Grok emits split command/args"
        );
        Some(NormalizedTerminalCommand {
            program: "/bin/bash",
            args: vec![shell_flag.clone(), script.clone()],
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn normalize(command: &str, args: &[String]) -> Option<NormalizedTerminalCommand> {
        normalize_terminal_command(command, args)
    }

    #[test]
    fn already_split_bash_request_is_unchanged() {
        let args = vec!["-lc".to_string(), "printf already-split".to_string()];

        assert!(normalize("/bin/bash", &args).is_none());
    }

    #[test]
    fn packed_grok_bash_request_normalizes_and_runs() {
        let normalized = normalize("/bin/bash -lc 'printf grok-ok'", &[])
            .expect("packed Grok bash request should normalize");

        assert_eq!(normalized.program, "/bin/bash");
        assert_eq!(normalized.args, ["-lc", "printf grok-ok"]);

        let output = std::process::Command::new(normalized.program)
            .args(&normalized.args)
            .output()
            .expect("normalized argv should spawn");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"grok-ok");
    }

    #[test]
    fn packed_long_script_spawns_as_an_argument() {
        let script = format!("true; # {}", "x".repeat(16 * 1024));
        let command = format!("/bin/bash -lc '{script}'");
        let normalized =
            normalize(&command, &[]).expect("long packed Grok bash request should normalize");

        let status = std::process::Command::new(normalized.program)
            .args(&normalized.args)
            .status()
            .expect("long script should not be used as the executable path");
        assert!(status.success());
    }

    #[test]
    fn packed_script_preserves_shell_syntax_newlines_and_unicode() {
        let script = "printf \"$HOME|quoted;世界\" | cat\nprintf \"\\nsecond line\"";
        let command = format!("/bin/bash -lc '{script}'");
        let normalized =
            normalize(&command, &[]).expect("quoted packed Grok bash request should normalize");

        assert_eq!(normalized.args, ["-lc", script]);
    }

    #[test]
    fn packed_double_quoted_payload_is_supported() {
        let normalized = normalize(r#"/bin/bash -lc "printf double-quoted""#, &[])
            .expect("double-quoted payload should normalize");

        assert_eq!(normalized.args, ["-lc", "printf double-quoted"]);
    }

    #[test]
    fn packed_bash_c_flag_is_supported() {
        let normalized = normalize("/bin/bash -c 'true'", &[]).expect("bash -c should normalize");

        assert_eq!(normalized.args, ["-c", "true"]);
    }

    #[test]
    fn packed_unquoted_payload_is_supported() {
        let normalized =
            normalize(r"/bin/bash -lc printf\ ok", &[]).expect("POSIX word should normalize");

        assert_eq!(normalized.args, ["-lc", "printf ok"]);
    }

    #[test]
    fn grok_request_with_existing_args_is_not_normalized() {
        let args = vec!["unexpected".to_string()];

        assert!(normalize("/bin/bash -lc 'printf nope'", &args).is_none());
    }

    #[test]
    fn malformed_or_extra_words_are_not_normalized() {
        assert!(normalize("/bin/bash -lc 'unterminated", &[]).is_none());
        assert!(normalize("/bin/bash -lc 'safe' extra", &[]).is_none());
        assert!(normalize("/bin/sh -lc 'wrong shell'", &[]).is_none());
        assert!(normalize("/bin/bash -x 'wrong flag'", &[]).is_none());
    }
}
