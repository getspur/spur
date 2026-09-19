# Grok terminal compatibility ownership

Issue: bd-28jrx. Base: cb3c16993. This follows the user's request to keep
Grok-specific terminal behavior under `crates/spur-acp/src/adapter/`.

The packed Bash command parser and its diagnostic currently live in the native
connection. Move both behind a crate-private adapter entry point, following the
existing function-based agent dispatch. Preserve the narrow Unix-only policy:
Grok, empty args, exact `/bin/bash`, `-c` or `-lc`, and one POSIX-lexed script
word. Other requests retain direct execution; do not broaden shell inference.

The native handler selects adapted or original argv, then owns process creation,
output, exit publication, kill, release, and shutdown. The inherited-pipe fix in
cb3c16993 remains shared: split argv reproduced the defect without normalization.
Add actual `AgentKind::Generic` coverage for that lifecycle, including same-session
continuation, rather than relying solely on the Grok split-argv control.

Move parser tests and byte-level goldens alongside adapter code. Keep the ACP
integration harness and corpus reusable, retaining the Grok packed-vs-split
oracle. Test adapter dispatch for every currently declared non-Grok agent.
Unrelated model-selection and notification compatibility is outside this change.

The solve catalog's `configuration.attribute_allowed_pair` verifies finite
ownership assignments: agent-specific argv/logging belong to the adapter;
shared lifecycle belongs to the connection. Initial verification
`sol_d59e83567e4c49ba` rejects the two current compatibility owners and accepts
the lifecycle owner. This checks supplied ownership facts, not source semantics;
source review and runtime goldens establish the implementation.

Acceptance: unchanged golden argv and script outcomes, explicit Generic lifecycle
regressions, no Grok parser/diagnostic in native terminal handling, full crate
tests and scoped strict Clippy, independent review, and a follow-up commit
without rewriting the base history.
