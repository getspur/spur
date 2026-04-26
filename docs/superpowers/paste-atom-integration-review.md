# Paste-as-Atom Integration Review (Upstream / Downstream)

> **Scope:** Upstream paste-capture routing and downstream post-submit rendering paths.  
> **Not reviewed:** The in-file `input_bar.rs` implementation (handled by focused codex review).  
> **Branch base:** `main` at review time; codex implementation branch not yet merged.

## Summary verdict

**YELLOW** — The design is mechanically sound, but three integration hazards (`!` prefix drift, dashboard parity, and unbounded paste storage) need explicit mitigation before merge. Most other concerns are pre-existing or cosmetic.

## Upstream concerns

### 1. Terminal compatibility — bracketed paste as progressive enhancement (acceptable)

`tui.rs` unconditionally emits `EnableBracketedPaste` on startup and `DisableBracketedPaste` on teardown. This is correct for crossterm and covers iTerm2, kitty, Alacritty, tmux, and most modern terminals. Terminal.app on older macOS versions historically ignored the sequence, but it is harmless.

**Fallback behavior:** Terminals that do not support bracketed paste emit raw keystrokes (individual `Event::Key` events, not `Event::Paste`). The atomization feature simply will not fire in those terminals — multi-line pastes arrive as discrete keystrokes. This is a graceful degradation, not a regression, **provided** the input bar does not treat an embedded `\r` or `\n` keystroke as `Enter`/`Submit` during the raw stream. `InputBar::handle_key` maps `KeyCode::Enter` to `HandleOutcome::Submit`, so a terminal that maps pasted newlines to `Enter` could prematurely submit. This is a pre-existing risk, not introduced by the feature, but the paste-as-atom design makes it more noticeable because users in supporting terminals will grow accustomed to safe multi-line pasting.

*Recommendation:* Document the bracketed-paste dependency in the user-facing help text so users of non-compliant terminals understand why multi-line pastes behave differently.

### 2. Routing parity — dashboard and session_detail both hit `InputBar::insert_paste`

Both views route `Event::Paste` into the same `InputBar::insert_paste` method:

- `DashboardView::handle_paste` → `self.input_bar.insert_paste(text)` (also forces `DashboardMode::Compose`).
- `SessionDetailView::handle_paste` → `self.input_bar.insert_paste(text)`.
- `SessionPickerView::handle_paste` consumes only `text.lines().next()` for rename / search filters — no atomization needed.

**Verdict:** If codex implements the atomization logic *inside* `InputBar::insert_paste` (or a helper called by it), both composing views get the feature automatically. If codex implements it at the view level (e.g. a new `InputBar::insert_paste_atom` called only from `session_detail.rs`), `DashboardView` will miss it.  
*Recommendation:* Confirm the implementation lives in `InputBar`, not in `SessionDetailView::handle_paste`.

### 3. Massive paste safety — no upstream size guard

`Event::Paste(text)` carries the full pasted string in a `String` allocation. There is no size cap in crossterm, in `app.rs:801`, or in the current `InputBar::insert_paste`. The design proposes storing originals in a `BTreeMap<usize, String>` keyed by a per-session counter. Without an eviction bound, repeated large pastes cause unbounded memory growth for the lifetime of the TUI process.

*Recommendation:* Add a hard cap (e.g. max 50 stored pastes, LRU eviction) and optionally reject individual pastes over a size threshold (e.g. 1 MB) with a status-bar warning.

### 4. Paste-during-other-modes — history state not restored, vim mode ignored

`SessionDetailView::handle_paste` and `DashboardView::handle_paste` both call `insert_paste` without checking `InputBar` state:

- **Vim Normal mode:** `insert_paste` writes directly into the `TextArea` without transitioning to Insert mode. The user will see text appear while the cursor style still indicates Normal mode. This is inconsistent with vim conventions where `p` in Normal mode pastes after the cursor.
- **History navigation (Ctrl+P / Ctrl+N):** `insert_paste` sets `history_cursor = None` but does **not** restore the stashed `draft` snapshot. If the user is browsing history and pastes, the paste is appended to the history entry currently on screen, and that mutated text becomes the new live draft. This is a pre-existing bug in `insert_paste`, but it becomes more acute with multi-line paste atoms because the user may accidentally submit a corrupted hybrid of history + paste.

*Recommendation:* In `insert_paste`, if `history_cursor` is `Some`, restore `self.draft` into the textarea before inserting the paste.

## Downstream concerns

### 5. Trace pane wrapping — cache invalidation is correct, no per-entry height cap

A 16-line paste becomes one `TraceKind::UserMessage` entry whose `text` field contains 15 `\n` characters. `ReactTrace::push_user_message` calls `self.react_trace.push(...)`, which bumps `generation` and sets `dirty_from`. Both the markdown and non-markdown render paths rebuild the cache from the dirty index onward.

In `builder.rs`, `TraceKind::UserMessage` iterates `entry.text.lines()` and pushes one `Line` per logical line, each prefixed with three spaces. `wrap_line_to_width` then splits each logical line into as many visual lines as needed for the pane width. There is **no per-entry height limit** — a 1000-line paste will generate 1000+ visual rows and push the scrollback anchor to the bottom. The global `MAX_LOG_ENTRIES = 5000` limits the number of *entries*, not their height.

*Verdict:* The cache invalidation is correct. The only risk is a pathologically tall user message swamping the viewport. This is acceptable for a chat interface, but consider whether a single-entry height clamp (e.g. first 100 lines + "… truncated") is desirable for pasted logs.

### 6. Copy-paste of the rendered message — copy-clean borders verified, recursion is benign

The trace pane uses `Borders::TOP | Borders::BOTTOM` only (no side borders), and the copy-friendly tests in `render.rs` assert that no `║` or `█` glyphs are written. A multi-line user message renders as:

```
─── top border ───
   💬 YOU
   first line of paste
   second line of paste
─── bottom border ───
```

If the user copies this rendered output (including the `   ` indent and border lines) and pastes it back, the pasted text contains newlines and therefore triggers a **new** paste atom. This is technically correct — the user pasted multi-line text — but the border artifacts and indentation become part of the new atom's payload. This is benign; it does not corrupt state or cause infinite recursion because the second render will show the same text again without self-referential expansion.

### 7. ACP serialization — newlines are preserved, no escape edge cases

The submit path is:

1. `InputBar::take_submit_capture` → `(expanded_text, ranges, interrupt)`.
2. `submit_router::route` → `assemble_blocks` interleaves `Text` and `ResourceLink` blocks.
3. `Action::SendMessage { blocks, interrupt }`.
4. `app.rs` forwards `UserInput::Message { blocks, interrupt }` to `spur_cli`.
5. `spur_cli` maps to `spur_core::InteractiveInput::Message`.
6. The orchestrator builds a `PromptRequest` with `Vec<ContentBlock>`.

`ContentBlock::Text(TextContent::new(s))` stores the raw string. JSON serialization via `serde_json` escapes literal newlines as `\n` and backslashes as `\\`. There is no risk of newline vs. `\n` ambiguity because the transport layer uses proper JSON string encoding. The `agent_client_protocol` crate handles deserialization on the agent side, restoring the original bytes.

### 8. Markdown rendering — user messages bypass markdown

Under `#[cfg(feature = "markdown")]`, only `TraceKind::AgentMessage` entries are fed through `MarkdownStream`. `TraceKind::UserMessage` is rendered in `builder.rs` as plain yellow text, line-by-line, with no markdown parsing. A pasted code block containing triple-backticks will therefore render **verbatim** (as the user intended), not as a styled fenced code block. This is the correct behavior for user messages — the user sees exactly what they sent.

If future designs want user messages to render as markdown, that would be a separate feature request, not a bug in paste-as-atom.

### 9. Slash-command interaction — trailing paste breaks slash resolution

`submit_router::route` checks `text.starts_with('/')` and then calls `registry.resolve(text)` on the **full** expanded text. If the user types `/clear` and then pastes a multi-line atom, the expanded text is `/clear<original_paste>`. The registry lookup is unlikely to match `/clear` because of the trailing paste content, so the command falls through to `SubmitDecision::Send` and is transmitted as plain text to the agent rather than executed locally.

This is a pre-existing fragility (any trailing text breaks slash matching), but paste-as-atom makes it more likely because users may not realize the atom expands into trailing text.  
*Recommendation:* Accept as known limitation; slash commands should be submitted before pasting.

### 10. `!` interrupt prefix — **REGRESSION RISK**

`InputBar::submit` currently computes:

```rust
let text = self.textarea.lines().join("\n");
let interrupt = text.starts_with('!');
self.submit_capture = Some((text.clone(), ranges.clone(), interrupt));
```

If the placeholder text is `[Paste #1 · 5 lines]`, `interrupt` is `false` even when the **expanded** text starts with `!`. The orchestrator later calls `strip_bang_prefix`, which strips the `!` from the first text block, but the `interrupt` boolean forwarded to the agent remains `false`. The result: the user pastes `!stop`, the agent receives `stop` without the interrupt signal, and the brain is not cancelled.

**This is the most severe integration risk identified.**

*Recommendation:* Ensure `submit()` (or `take_submit_capture`) recomputes `interrupt` from the **expanded** text, not the placeholder text. A unit test covering `!` inside a paste atom is essential.

## Cross-cutting risks

- **Unbounded memory:** The per-session paste store must have a cap (see Upstream #3).
- **History corruption:** `insert_paste` corrupts the draft when fired during history browsing (see Upstream #4). This is pre-existing but becomes more painful with large paste atoms.
- **Interrupt flag drift:** The `!` prefix logic is decoupled from expansion, creating a silent semantic mismatch (see Downstream #10).

## Recommended pre-merge checks

1. **Unit test:** Paste atom containing `!stop` → assert `interrupt == true` in the captured submit tuple.
2. **Unit test:** Paste atom starting with `/clear` followed by more text → assert `SubmitDecision::Local` is still produced (or document the limitation).
3. **Memory bound:** Verify the paste storage map has a max capacity and evicts oldest entries.
4. **Dashboard parity:** Confirm `DashboardView::handle_paste` exhibits the same atomization behavior as `SessionDetailView::handle_paste` without code duplication.
5. **History hygiene:** Verify that pasting while browsing history restores the draft snapshot before inserting.
6. **End-to-end trace render:** Submit a 50-line paste atom and verify the trace pane renders all lines, scrolls to bottom, and the position indicator updates correctly.
