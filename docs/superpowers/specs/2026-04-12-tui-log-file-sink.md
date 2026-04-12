# TUI Log File Sink

## Problem

`tracing_subscriber::fmt::init()` in `crates/spur-cli/src/main.rs:115` sets up a global tracing subscriber that writes to stdout. When the TUI is active (`spur watch`), ratatui uses stdout for its alternate screen buffer. Tracing output (e.g., `INFO spur_core::orchestrator: Spawning brain session...`) writes raw text over the TUI, corrupting the display.

## Solution

Replace the single `tracing_subscriber::fmt::init()` call with a mode-aware initializer that routes tracing output to a log file when the TUI is active.

## Design

### `init_tracing(tui_mode: bool)` in `spur-cli/src/main.rs`

- **`tui_mode = false`** (commands: `run`, `init`, `agents`): Write to stdout as today. No behavior change.
- **`tui_mode = true`** (command: `watch`): Write to `.spur/logs/spur-<YYYY-MM-DD>.log` under the repo root. No stdout/stderr output.

### Log file details

- **Location**: `<repo_root>/.spur/logs/spur-<YYYY-MM-DD>.log`
- **Directory creation**: Create `.spur/logs/` if it doesn't exist (the `.spur/` directory already exists in the project).
- **Writer**: Use `tracing_appender::rolling::daily()` for date-based rotation and non-blocking I/O via `tracing_appender::non_blocking()`.
- **Format**: Keep the default `tracing_subscriber::fmt` format (timestamp, level, target, message).

### New dependency

- `tracing-appender` added to `crates/spur-cli/Cargo.toml`.

### Changes required

1. **`crates/spur-cli/Cargo.toml`**: Add `tracing-appender` dependency.
2. **`crates/spur-cli/src/main.rs`**:
   - Remove the existing `tracing_subscriber::fmt::init()` call at line 115.
   - After `Cli::parse()`, determine `tui_mode` from the parsed command variant (true for `Commands::Watch`, false otherwise).
   - Call `init_tracing(tui_mode, &repo_root)` before the `match cli.command` dispatch.
   - Implement `init_tracing()` using a layered subscriber with conditional writer.

### What does NOT change

- `SpurEvent` enum and broadcast channel.
- `ReactTrace`, `ActivityLog`, or any TUI component.
- Non-TUI command output behavior.
- The `.gitignore` — `.spur/` is already present.
