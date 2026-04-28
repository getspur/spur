# spur-bot Remediation Spec — v3.1

> **Status:** v3.1 — gated for implementation. Codex gate-3 (bd-3qe.3.5) flagged two deterministic spec edits (reqwest version qualifier, saturating_add stale arithmetic); gemini gate-3 (bd-3qe.3.6) returned GATE PASS. v3.1 applies both edits with no design change.
> **Parent epic:** bd-3qe.3
> **Source review:** bd-3qe (gemini bd-3qe.2 + codex bd-3qe.1)
> **Scope:** `crates/spur-bot/src/`

Gate-3 outcome (8/8 gated): all 8 fixes (C0, C1, C2, H3, H4, H5, H6, H7, H8) ready for implementation per the build sequence below.

v3 → v3.1 changelog (deterministic spec edits, no design change):
- C0 sketch: qualified `reqwest::ClientBuilder` → `frankenstein::reqwest::ClientBuilder`. spur-bot's workspace reqwest is 0.12; frankenstein 0.49 uses reqwest 0.13. Passing a workspace-reqwest client into the frankenstein builder won't type-check. The re-export is verified at `frankenstein-0.49.0/src/lib.rs:6`.
- C2 sketch: `timeout_secs + 10` → `timeout_secs.saturating_add(10)` (two occurrences). C2 lands after C0; the stale arithmetic would silently regress C0's overflow guarantee.

Gate-2 outcome (6/8 dual-approved): **C1, C2, H3, H6, H7, H8 ✅ APPROVE**. C0 dual-blocked, H4 dual-revise, H5 split-revise. v3 re-opened those three.

v2 → v3 changelog:
- **C0 redesigned**. Both reviewers (correctly) blocked v2: (a) `frankenstein 0.49` does NOT have `Bot::with_client` — actual API is `Bot::builder().api_url(...).client(http).build()`; (b) `frankenstein 0.49` ALREADY ships `connect_timeout=10s, timeout=500s` defaults (verified at `frankenstein-0.49.0/src/client_reqwest.rs:20-29`), so v2's "no-timeout footgun" premise was factually wrong; (c) sharing a single `reqwest::Client` and `.clone()`-ing the `TelegramClient` couples poll-and-send timeouts — a 30s global hard-timeout severs any long-poll >30s. v3 uses **two separate `TelegramClient` instances** (one per timeout regime) and uses `saturating_add` for overflow safety.
- **H4 budget bug fixed**. Both reviewers caught the same off-by-N: budget subtracted fixed `"\n\n…[truncated]"` (≈14 units) but actual tail is dynamic `"\n\n…[truncated; {N} chars dropped]"` (up to ~36+ units). v3 computes the actual tail before choosing the budget.
- **H5 false rationale corrected**. v2 claimed `sender.rs:22` already logs per-draft errors; codex verified this is wrong (`let _ = ...await` at sender.rs:23). v3 folds the one-line `tracing::warn!` addition into H5 scope.
- Build verification commands switched from `cargo` to `scripts/spur-cargo` per repo convention.

v1 → v2 changelog (preserved for history):
- Added C0: TCP half-open deadlocks (gemini's missed mode).
- C1: dropped fatal-panic; added broadcast `Lagged/Closed` handling.
- C2: removed `is_ok()` swallow; closure error propagates.
- H3: enumerated full async-ripple call graph.
- H4: REJECTED → redesigned (single-message + byte-aware truncate).
- H5: scoped narrowed.
- H7: path/operation context.
- H8: `tempfile` dev-dep → prod-dep.
- Build sequence: H8+H3 squashed; C0 first.

---

## Reviewer-gate protocol (unchanged)

Each fix needs `APPROVE` from BOTH reviewers. `REVISE` triggers respin. `REJECT` blocks pending redesign.

---

## C0 — TCP half-open deadlocks + timeout regime split (REDESIGNED in v3)

### Evidence (verified)
- `crates/spur-bot/src/telegram/client.rs:11` — `frankenstein::client_reqwest::Bot::new(token)` is used.
- `frankenstein-0.49.0/src/client_reqwest.rs:10-18` — `Bot` is a `bon::Builder`-derived struct with public `api_url` and `client` fields. Real builder API is `Bot::builder().api_url(...).client(reqwest_client).build()`.
- `frankenstein-0.49.0/src/client_reqwest.rs:20-29` — default client already configures `connect_timeout=10s` and `timeout=500s`. **Not** "no timeout" as v2 implied.

Real failure modes (refined):
- **A blackholed long-poll** sits in the 500s default before `get_updates` returns `reqwest::Error` (Timeout). 500s is too long for a healthy heartbeat; during this period the poll loop is single-threaded-frozen.
- **A blackholed outbound call** (`send_text_to_thread`, `create_forum_topic`, `answer_callback`) sits in the same 500s — the main `select!` loop can't service its other arms while a sluggish-or-dead `await` holds the task.
- **Operationally**, 500s is fine for long-poll (server-side `timeout=30s` + slack) but absurdly long for a fast outbound call.

The right model is **two timeout regimes** because they have different physics:
- Polling: must allow the server-side long-poll to elapse naturally. `client_total_timeout > poll_timeout_secs + slack`.
- General calls: should fail fast (≈30s) so the main loop doesn't stall on a flaky connection.

### Root cause (first principles)
v2 confused two distinct requirements (long-tolerant poll vs. fast-fail RPC) into one configuration. Both reviewers correctly identified that sharing one `reqwest::Client` (or cloning a `TelegramClient` whose inner `Bot` shares the same `reqwest::Client`) makes the regime split impossible: the shorter timeout always wins.

### Candidate fixes (v3)

**A. Two `TelegramClient` instances with independent `reqwest::Client`s.**
- Pros: clean separation; each call site reaches for the right client; matches the dual-regime physics.
- Cons: callers (`mod.rs`) must hold both; minor surface area.

**B. One `TelegramClient` with two internal clients, switched per method.**
- Pros: callers don't change.
- Cons: hides the regime split inside the abstraction; surprises readers; method-level switch table to maintain.

**C. Single client with very long timeout, all sites wrapped in `tokio::time::timeout`.**
- Pros: one client.
- Cons: ugly; easy to forget a wrap on a new method; doesn't actually disable the broken default 500s for fast calls.

### Chosen: **A** — two `TelegramClient` instances.

Rationale: explicit beats implicit. Callers in `mod.rs` already deal with separate concerns (`poll_client.clone()` is passed to the spawned poll task, the main loop uses `client` for everything else); making the timeout split visible matches that structure.

Sketch:
```rust
// telegram/client.rs
impl TelegramClient {
    /// `request_timeout` should be:
    ///   - ~30s for fast-fail RPC calls (send/create/answer)
    ///   - poll_timeout_secs + 10s for long-poll
    pub fn new(token: &str, request_timeout: std::time::Duration) -> anyhow::Result<Self> {
        // Use frankenstein's re-export to avoid type-mismatch: spur-bot's
        // workspace `reqwest = "0.12"` is incompatible with frankenstein 0.49's
        // internal `reqwest 0.13`; passing a 0.12 client into the frankenstein
        // builder won't type-check.
        let http = frankenstein::reqwest::ClientBuilder::new()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(request_timeout)
            .build()
            .context("building reqwest client for telegram bot")?;
        let api_url = format!("{}{}", frankenstein::BASE_API_URL, token);
        Ok(Self {
            inner: frankenstein::client_reqwest::Bot::builder()
                .api_url(api_url)
                .client(http)
                .build(),
        })
    }
}

// telegram/mod.rs
const RPC_TIMEOUT_SECS: u64 = 30;
let request_timeout = std::time::Duration::from_secs(RPC_TIMEOUT_SECS);
let poll_timeout = std::time::Duration::from_secs(cfg.poll_timeout_secs.saturating_add(10));
let client = client::TelegramClient::new(cfg.bot_token.as_deref().expect("validated"), request_timeout)?;
let poll_client = client::TelegramClient::new(cfg.bot_token.as_deref().expect("validated"), poll_timeout)?;
// ... `poll_client` moves into the spawned poll task; `client` stays in the main loop.
```

Long-poll outer-deadline (defense in depth) inside `poll_loop.rs`:
```rust
let poll_deadline = std::time::Duration::from_secs(timeout_secs.saturating_add(10));
let result = tokio::time::timeout(poll_deadline, client.get_updates(offset, timeout_secs)).await;
match result {
    Ok(Ok(batch)) => { /* normal path */ }
    Ok(Err(error)) => { /* existing transient HTTP-error backoff */ }
    Err(_elapsed) => {
        tracing::warn!(secs = poll_deadline.as_secs(), "long-poll exceeded outer deadline; rotating connection");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
    }
}
```

Note: the inner `reqwest` timeout for the poll client (`poll_timeout_secs + 10s`) is INTENTIONALLY equal to the outer Tokio deadline. They're redundant by design — defense in depth. If reqwest cuts off first you get `Ok(Err(Timeout))` and existing backoff logic; if Tokio cuts off first you get `Err(Elapsed)` and the same backoff. Either path keeps the loop alive.

### Acceptance criteria
- Test (`tokio::net::TcpListener` that accepts and never responds): `client.send_text_to_thread` errors within `request_timeout + 1s` on the general client; `poll_client.get_updates` errors within `poll_timeout_secs + 10s + 1s`.
- Both clients are constructed in `mod.rs` from the same token; only the timeout differs.
- Overflow safety: `cfg.poll_timeout_secs = u64::MAX` does not panic at startup.
- Bot continues to make subsequent calls after a stall.

### Open questions for re-gate
- Should `request_timeout` be exposed via `TelegramBotConfig`? Recommend YES with default 30s.
- `cfg.bot_token.as_deref().expect("validated")` is called twice in the sketch — fine (the `Option` is cheap to read), but a worker may prefer to bind once.

---

## C1 — Bot crashes on transient errors in main `select!` loop

### Evidence
`crates/spur-bot/src/telegram/mod.rs:51–148` — `?` propagates from `runtime.handle_chat_text`, `client.create_forum_topic`, `client.send_text_to_thread`, `render::render_batch_to_thread`, `runtime.flush_pending`, `runtime.handle_spur_event`, `runtime.handle_permission_request`. Affected sites: lines 65, 68, 74, 80, 95, 113, 119, 122, 133, 137, 145.

### Root cause (first principles)
Two coupled defects:
1. `?` treats every error as fatal; a single transient kills the process.
2. `event_rx.recv()` returns `tokio::sync::broadcast::Result<Event>`. The current `Ok(event) = event_rx.recv() => ...` arm **silently drops both `Lagged(n)` and `Closed`** — `Closed` should exit the loop; `Lagged` should warn-and-continue.

### Candidate fixes

**A. Per-arm error wrapper (no fatal escape).** Each `select!` arm dispatches into a `process_*` helper returning `anyhow::Result<()>`. Loop logs `Err` and continues. Channel-close exits via `None`/`Closed`. No panics, no fatal-error sentinel.
- Pros: simplest, default-safe.
- Cons: `host.shutdown()` errors at the END of the loop are still propagated — but those are post-loop and out of scope.

**B. As above + `BotError::Fatal` sentinel.** Hybrid from v1.
- Cons: adds a second error-channel for ~zero benefit since gemini & codex agree there are no in-loop fatal conditions besides channel close.

### Chosen: **A** (gemini + codex agreed on dropping the fatal escape hatch).

Sketch:
```rust
loop {
    tokio::select! {
        maybe_update = update_rx.recv() => {
            let Some(inputs) = maybe_update else { break; };
            for input in inputs {
                if let Err(err) = process_input(&mut runtime, &handle, &client, &sender, input).await {
                    tracing::error!(error = ?err, "transient error handling telegram input");
                }
            }
        }
        event = event_rx.recv() => {
            match event {
                Ok(event) => {
                    if let Err(err) = process_spur_event(&mut runtime, &handle, &client, &sender, event).await {
                        tracing::error!(error = ?err, "transient error handling spur event");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "telegram bot lagged on spur event broadcast");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        Some(request) = perm_rx.recv() => {
            if let Err(err) = process_permission(&mut runtime, &client, &sender, request).await {
                tracing::error!(error = ?err, "transient error handling permission request");
            }
        }
        _ = cancellation.cancelled() => {
            tracing::info!("cancellation signaled; winding down");
            break;
        }
    }
}
```

The `cancellation.cancelled()` arm is added now (used by H5).

### Acceptance criteria
- A simulated transient send failure does NOT exit `run_telegram_bot`; the next inbound update is still processed.
- An induced broadcast lag (slow consumer + producer firing many events) emits a `warn` log with the lag count, no crash.
- Channel-close on `update_rx` or `Closed` on `event_rx` still breaks the loop cleanly.
- Tracing emits `error` level for every swallowed transient.
- No new panics introduced.

---

## C2 — Poll-loop CPU spin under `update_tx` backpressure

### Evidence
`crates/spur-bot/src/telegram/mod.rs:43–45` — `update_tx.try_send(inputs)?` inside the batch callback.
`crates/spur-bot/src/telegram/poll_loop.rs:30–32` — `let accepted = on_batch(batch).is_ok(); offset = advance_offset(offset, &ids, accepted); backoff = 250ms;`. The `Ok(_)` arm resets backoff regardless of `accepted`.

### Root cause (first principles)
Same as v1: backpressure isn't propagated; offset advancement and HTTP success are conflated. Codex correctly identified that even with async `send().await`, swallowing the closure's `Err` via `is_ok()` keeps a closed-channel scenario in a tight loop.

### Chosen: async `send().await` + propagate closure error

The closure CAN'T fail except on a closed channel (since `router::normalize_update` is infallible and `send().await` only errors when the receiver is dropped). Closed channel = main loop has exited = poll loop should also exit. So propagating the error is correct.

Sketch:
```rust
// mod.rs
let result = poll_loop::run_poll_loop(&poll_client, cfg_poll_timeout, poll_cancellation_for_poll, |batch| {
    let update_tx = update_tx.clone();
    async move {
        let mut inputs = Vec::new();
        for update in batch {
            if let Some(input) = router::normalize_update(&update, operator_user_id) {
                inputs.push(input);
            }
        }
        if !inputs.is_empty() {
            update_tx.send(inputs).await
                .map_err(|_| anyhow::anyhow!("update channel closed"))?;
        }
        Ok(())
    }
})
.await;
// H5 handles the spawn-result logging here.
```

`run_poll_loop` becomes:
```rust
pub async fn run_poll_loop<F, Fut>(
    client: &TelegramClient,
    timeout_secs: u64,
    cancellation: CancellationToken,
    mut on_batch: F,
) -> anyhow::Result<()>
where
    F: FnMut(Vec<Update>) -> Fut + Send,
    Fut: Future<Output = anyhow::Result<()>> + Send,
{
    client.delete_webhook().await?;
    let mut offset = 0_i64;
    let mut backoff = std::time::Duration::from_millis(250);

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            result = tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs.saturating_add(10)),
                client.get_updates(offset, timeout_secs),
            ) => {
                match result {
                    Ok(Ok(batch)) => {
                        let ids: Vec<i64> = batch.iter().map(|u| u.update_id as i64).collect();
                        on_batch(batch).await?;  // closed channel terminates loop
                        offset = advance_offset(offset, &ids, true);
                        backoff = std::time::Duration::from_millis(250);
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "telegram poll failed");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                    }
                    Err(_elapsed) => {
                        tracing::warn!(secs = timeout_secs.saturating_add(10), "long-poll exceeded deadline");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                    }
                }
            }
        }
    }
}
```

`advance_offset`'s `accepted` parameter becomes vestigial. Either remove it (clean up unused param + tests) or keep with `true` always — recommend the cleanup.

### Acceptance criteria
- Test that fills `update_rx` capacity and verifies `get_updates` is NOT called more than once per pending consumer wake-up.
- Closing the update receiver causes `run_poll_loop` to return `Err(_)` cleanly.
- `advance_offset` only advances after `on_batch` returns `Ok`.
- Long-poll timeout test from C0 still passes.

---

## H3 — Blocking `std::fs::write` in async paths (now fully scoped)

### Evidence
`crates/spur-bot/src/state.rs:117–121` — sync I/O.
Save call sites in `runtime.rs`: **lines 206, 332, 430, 505, 603**.
**Async-ripple call graph**:
- `ensure_topic_record` (`runtime.rs:185`) — currently sync; called from `mod.rs:75` (async context). Must become async.
- `ensure_known_topic` (`runtime.rs:310`) — currently sync; called from `runtime.rs:345, 445`. Must become async.
- `handle_spur_event` (`runtime.rs:560`) — currently sync, returns `Result<(_, Vec<RuntimeRender>)>`; called from `mod.rs:119` (async context, already inside `select!`). Must become async.
- `handle_chat_text`, `handle_callback`, `handle_permission_request` — already async, no signature change.

### Root cause (first principles)
Same as v1.

### Chosen: combine with H8's `spawn_blocking` strategy.

`save` becomes async, internally uses `spawn_blocking` for the tempfile write (H8 implementation). All save callers become async via `.await`. Sync helpers (`ensure_topic_record`, `ensure_known_topic`, `handle_spur_event`) are async-ified.

This is a single compile-green commit alongside H8 (per build sequence below).

Sketch (caller-side ripple):
```rust
// runtime.rs
pub async fn ensure_topic_record(&mut self, ...) -> anyhow::Result<()> {
    // ... existing logic ...
    if inserted {
        self.state_store.save(&self.persistable_state()).await?;
    }
    Ok(())
}

async fn ensure_known_topic(&mut self, key: &ThreadKey) -> anyhow::Result<()> { ... }

pub async fn handle_spur_event(&mut self, event: SpurEvent) -> anyhow::Result<(Option<ThreadKey>, Vec<RuntimeRender>)> {
    // body unchanged structurally; .await on save and ensure_known_topic
}

// mod.rs:75 — already inside an async block, just .await the new async fn
```

### Acceptance criteria
- All five save call sites use `.await`.
- Tests that previously called these synchronously update to `await`.
- Tokio test with a slow-blocking save mock (using `tokio::time::sleep` inside `spawn_blocking`) confirms the runtime task can still service other `select!` arms.

---

## H4 — Truncation: REJECTED v1 → REDESIGNED

### Evidence
gemini REJECT: chunked sends will trip Telegram's per-topic rate limit (≈20 messages/min in supergroups/topics). A 50K-char agent answer would queue 13 messages and likely 429-throttle the bot.
codex REVISE: char-cap is wrong unit; buttons need byte-cap; text needs UTF-16-unit cap.

### Root cause (first principles)
v1 conflated "preserve every character of the agent's answer" with "stay below the API limit". The right trade-off for a chat surface is **truncate-with-indicator + escape hatch** (the operator can `/resume` and re-query for full text, or read the session log offline).

### New design: single message, byte-aware truncate, "(truncated)" indicator

**Text rule**: if `text.encode_utf16().count() <= 4096`, send as-is. Otherwise truncate to fit `4096 - actual_tail_units` UTF-16 units on a char boundary, append the indicator with the count of dropped chars. **Critical: the tail is dynamic** — `"\n\n…[truncated; {N} chars dropped]"` grows by ~one UTF-16 unit per decimal digit of N. v2 budgeted for the static template only and could overflow by up to ~20+ units for large drops. v3 computes the worst-case tail length from `text.chars().count()` BEFORE choosing the budget (since dropped ≤ total chars, the digit width is bounded).

**Button rule**: if `label.len() <= 64` (bytes), keep. Otherwise truncate to ≤61 bytes on a char boundary, append `"…"` (3 bytes UTF-8) for a final ≤64 bytes.

Helpers needed in `format.rs`:
```rust
/// Truncate `text` so its UTF-16 code-unit length is at most `max_units`.
/// Always cuts on a char boundary. Returns (kept, dropped_chars_count).
pub fn truncate_to_utf16_units(text: &str, max_units: usize) -> (String, usize) {
    let total = text.chars().count();
    let mut units = 0;
    let mut keep = String::with_capacity(text.len().min(max_units * 2));
    for ch in text.chars() {
        let next = units + ch.len_utf16();
        if next > max_units { break; }
        units = next;
        keep.push(ch);
    }
    let dropped = total - keep.chars().count();
    (keep, dropped)
}

/// Truncate `label` so its UTF-8 byte length is at most `max_bytes`.
/// Always cuts on a char boundary. Adds an ellipsis if truncated.
pub fn truncate_button_label_bytes(label: &str, max_bytes: usize) -> String {
    if label.len() <= max_bytes { return label.to_string(); }
    let cap = max_bytes.saturating_sub("…".len()); // 3 bytes
    let mut out = String::with_capacity(cap);
    for ch in label.chars() {
        if out.len() + ch.len_utf8() > cap { break; }
        out.push(ch);
    }
    out.push('…');
    out
}
```

Existing `split_for_telegram` and `short_button_label` stay for now (used in tests / future), but renderer calls the new byte-/unit-aware helpers.

Render-side application (v3 — budget computed against the actual maximum tail):
```rust
const TG_TEXT_LIMIT: usize = 4096;
const TG_BUTTON_LIMIT: usize = 64;

/// Returns the rendered body, single-message, ≤ TG_TEXT_LIMIT UTF-16 units.
fn render_truncated_text(text: &str) -> String {
    if text.encode_utf16().count() <= TG_TEXT_LIMIT {
        return text.to_string();
    }
    // Worst-case tail length: dropped ≤ total chars, so use the total-char
    // count to size the digit width pessimistically. This guarantees the
    // final body fits regardless of the actual dropped count.
    let total_chars = text.chars().count();
    let worst_tail = format!("\n\n…[truncated; {total_chars} chars dropped]");
    let worst_tail_units = worst_tail.encode_utf16().count();
    let budget = TG_TEXT_LIMIT.saturating_sub(worst_tail_units);
    let (kept, dropped) = format::truncate_to_utf16_units(text, budget);
    let actual_tail = format!("\n\n…[truncated; {dropped} chars dropped]");
    debug_assert!((kept.encode_utf16().count() + actual_tail.encode_utf16().count()) <= TG_TEXT_LIMIT);
    format!("{kept}{actual_tail}")
}

RuntimeRender::ServiceMessage { text } | RuntimeRender::FinalAnswer { text } => {
    let body = render_truncated_text(&text);
    client.send_text_to_thread(chat_id, message_thread_id, body).await?;
}

RuntimeRender::ReviewPrompt { text, buttons } | RuntimeRender::PermissionPrompt { text, buttons } => {
    let buttons: Vec<_> = buttons.into_iter()
        .map(|b| Button { label: format::truncate_button_label_bytes(&b.label, TG_BUTTON_LIMIT), ..b })
        .collect();
    let body = render_truncated_text(&text);
    client.send_buttons_to_thread(chat_id, message_thread_id, body, &buttons).await?;
}
```

Why "worst-case = total-char digit width" works: the actual dropped count is always ≤ total chars, so its decimal-digit width is ≤ that of total chars. The budget is therefore strictly large enough; the actual tail is strictly shorter than or equal to the worst-case tail. The `debug_assert!` makes the invariant explicit during tests.

### Acceptance criteria
- A 10,000-char agent answer renders as exactly ONE message ending with `…[truncated; …]`.
- **Budget invariant**: a property test feeding `render_truncated_text` with random texts of length 0–100,000 chars asserts `result.encode_utf16().count() <= 4096` for every output. This catches the v2 budget bug.
- Button labels >64 bytes (incl. multi-byte/emoji cases) truncate to ≤64 bytes with no panic from `String::truncate` byte-vs-char misalignment.
- Test the byte-edge: "🦀" (4 bytes UTF-8, 2 UTF-16 code units) repeated 20 times — verify both helpers don't slice mid-codepoint.
- Bot does not get rate-limited on long answers (single-send keeps us under 20/min).

---

## H5 — Detached spawn-result failures swallowed (scope corrected in v3)

### Evidence (verified in v3)
- `crates/spur-bot/src/telegram/mod.rs:34–49` — `let _ = run_poll_loop(...).await;` (poll-task spawn).
- `crates/spur-bot/src/telegram/sender.rs:22-31` — `tokio::spawn(async move { let _ = client.send_message_draft_to_thread(...).await; });` per-draft. **v2 incorrectly claimed errors were already logged here; codex verified they are not.**

### Root cause (first principles)
Two distinct spawn sites both discard their result. v2 punted the sender to "out of scope, already logged" — but the sender genuinely doesn't log either, so the punt was based on a false premise. Cheaper to fix both in one commit than to track a follow-up.

### Chosen: log at BOTH spawn sites; cancel-on-error only at the poll-loop site.

The poll-loop is load-bearing — its termination must wake the main loop. The sender's per-draft tasks are individually best-effort (the next 400ms tick can re-attempt or supersede the draft); they need logging but not cancellation propagation.

Sketch (poll-loop, mod.rs):
```rust
let poll_cancellation_for_main = cancellation.clone();
let poll_cancellation_for_loop = cancellation.clone();
tokio::spawn(async move {
    let result = run_poll_loop(&poll_client, cfg_poll_timeout, poll_cancellation_for_loop, |batch| {
        let update_tx = update_tx.clone();
        async move { /* see C2 */ }
    }).await;
    match result {
        Ok(()) => tracing::info!("telegram poll loop terminated cleanly"),
        Err(err) => tracing::error!(error = ?err, "telegram poll loop terminated unexpectedly"),
    }
    poll_cancellation_for_main.cancel();
});
```

Sketch (sender, sender.rs:22):
```rust
tokio::spawn(async move {
    if let Err(err) = client
        .send_message_draft_to_thread(
            update.chat_id,
            update.message_thread_id,
            &update.draft_id,
            &update.text,
        )
        .await
    {
        tracing::warn!(error = ?err, draft_id = %update.draft_id, "telegram draft send failed");
    }
});
```

Main loop's new `cancellation.cancelled()` arm (added in C1) catches the poll-loop cancel signal and breaks.

### Acceptance criteria
- A simulated `delete_webhook` failure causes the bot to log `error` and exit cleanly within 1s.
- A simulated `send_message_draft_to_thread` failure (e.g., 400 from the mock server) emits a `warn` log with the draft_id.
- `let _ =` removed from BOTH the poll-loop spawn site and the sender draft-send spawn site.

---

## H6 — Late fresh `AgentSessionReady` rebound to lobby ✅ APPROVED v1

### Evidence
`crates/spur-bot/src/runtime.rs:585–593` — lobby fallback for fresh sessions.
gemini & codex confirmed: lobby fallbacks at `runtime.rs:671` (review prompts) and `runtime.rs:818` (permission prompts) are independent and should remain.

### Chosen: drop with `tracing::warn!`. (no change from v1)

```rust
let key = if resumed { ... } else {
    let mut chosen = None;
    while let Some(candidate) = self.pending_new_session_keys.pop_front() {
        if self.pending_new_session_guard.remove(&candidate) {
            chosen = Some(candidate);
            break;
        }
    }
    let Some(key) = chosen else {
        tracing::warn!(%acp_session_id, "AgentSessionReady arrived with no eligible pending topic; dropping");
        return Ok((None, vec![]));
    };
    key
};
```

### Acceptance criteria
- Test: pending_new_session_keys exhausted by /resume eviction → fresh AgentSessionReady arrives → returns `(None, vec![])`; `session_threads` does not gain a lobby entry.
- Existing happy-path tests pass.

---

## H7 — Corrupt state silently reset (now with path/operation context)

### Evidence
`crates/spur-bot/src/runtime.rs:122` — `state_store.load().unwrap_or_default()`.
`crates/spur-bot/src/state.rs:82–113` — `load()` returns errors without operation/path context.

### Root cause (first principles)
Silent default-on-error wipes data; raw `serde_json` errors don't tell the operator WHICH file or WHICH operation failed.

### Chosen: bubble the error AND wrap with context in `state.rs`.

`state.rs::load`:
```rust
use anyhow::Context;

pub fn load(&self) -> anyhow::Result<PersistedBotState> {
    if !self.path.exists() {
        return Ok(PersistedBotState::default());
    }

    let raw = std::fs::read_to_string(&self.path)
        .with_context(|| format!("reading state file {}", self.path.display()))?;

    if let Ok(state) = serde_json::from_str::<PersistedBotState>(&raw) {
        return Ok(state);
    }

    let legacy: LegacyPersistedBotState = serde_json::from_str(&raw)
        .with_context(|| format!("parsing state file {} (current and legacy schemas both failed)", self.path.display()))?;
    /* ... legacy migration ... */
}
```

`runtime.rs::new`:
```rust
pub fn new(state_store: BotStateStore) -> anyhow::Result<Self> {
    let persisted = state_store.load()
        .context("loading persisted bot state; refusing to start with empty state")?;
    /* ... rest ... */
}
```

`mod.rs:19`:
```rust
let mut runtime = crate::runtime::BotRuntime::new(crate::state::BotStateStore::new(state_path))
    .context("initializing bot runtime")?;
```

### Acceptance criteria
- A pre-seeded corrupt-JSON state file causes `run_telegram_bot` to return `Err` with a chain that includes the path and the words "parsing state file".
- A non-existent state file still results in clean default state.
- An unreadable state file (e.g., permission denied) yields an error chain with "reading state file".

---

## H8 — Non-atomic state writes ✅ APPROVED v1 (with one note)

### Evidence
`crates/spur-bot/src/state.rs:120` — direct `std::fs::write`. `tempfile = "3"` is currently a **dev-dep only** in `crates/spur-bot/Cargo.toml:25`.

### Chosen: tempfile + same-directory persist via `spawn_blocking`.

Cargo.toml change required: move `tempfile = "3"` from `[dev-dependencies]` to `[dependencies]`.

```rust
pub async fn save(&self, state: &PersistedBotState) -> anyhow::Result<()> {
    if let Some(parent) = self.path.parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("creating state parent dir {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(state)?;
    let path = self.path.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("creating temp file in {}", dir.display()))?;
        std::io::Write::write_all(&mut tmp, &json)
            .with_context(|| "writing state to temp file")?;
        tmp.as_file().sync_all().with_context(|| "fsync temp file")?;
        tmp.persist(&path).map_err(|e| anyhow::anyhow!("renaming temp file to {}: {e}", path.display()))?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("save task join error: {e}"))??;
    Ok(())
}
```

`save` is now async — H3 ripple applies; both fixes land in the same commit.

### Acceptance criteria
- Killing the process between `write_all` and `persist` leaves the original `<path>` intact.
- After a successful save, a fresh `load()` returns the saved state.
- A test using `tempfile::tempdir()` writes 100 saves with random kill points and verifies the file is always either old-valid or new-valid, never corrupt.
- All errors include the path and operation in their context chain.

---

## Cross-cutting open questions for re-gate

1. **TCP timeout config plumbing.** Should `request_timeout` (default 30s) and `connect_timeout` (default 10s) be exposed via `TelegramBotConfig`? Recommend YES with the defaults shown.
2. **`advance_offset` cleanup.** With `accepted=true` always (post-C2), the parameter is dead. Remove it (and its unit tests) as part of C2's commit, or leave for a follow-up?
3. **(Resolved in v3)** ~~frankenstein API~~ — verified: builder API is `Bot::builder().api_url(...).client(http).build()`; `client` field is `pub`.
4. **(Resolved in v3)** ~~Sender logging audit~~ — verified: sender does NOT log; fix included in H5 v3.

---

## Build sequence (post-gate)

Reordered per reviewer feedback. Each step is one commit; type-check + tests must pass at each step:

1. **C0** — TCP timeouts (global reqwest config + long-poll wrapper). Foundational; protects the rest of the work. Adds one fallible signature on `TelegramClient::new`.
2. **H7** — `state.rs::load` context + `runtime.rs::new` returns Result. Small, no signature ripple to async.
3. **H8 + H3** — atomic write + async save (ONE commit, both reviewers required). Async-ripples through `ensure_topic_record`, `ensure_known_topic`, `handle_spur_event`. Cargo.toml moves `tempfile` to prod deps.
4. **H4** — truncation helpers + render-side application. No signature changes; pure addition of helpers + one-line replacements in `render.rs`.
5. **C2** — `run_poll_loop` async closure signature, error propagation, `advance_offset` cleanup. mod.rs callsite updated.
6. **H5** — poll-task spawn logs + cancel-on-error. Adds the new `cancellation.cancelled()` arm to main loop (preview of C1).
7. **C1** — main `select!` per-arm error wrappers + broadcast `Lagged/Closed` handling + `cancellation.cancelled()` arm finalization.
8. **H6** — drop late fresh AgentSessionReady. Smallest diff; lands anywhere after C1, scheduled last for clarity.

Build verification at each step (use repo wrapper per convention):
- `scripts/spur-cargo check -p spur-bot`
- `scripts/spur-cargo test -p spur-bot`
- `scripts/spur-cargo clippy -p spur-bot --all-targets -- -D warnings`

---

## Out of scope (unchanged)

The 11 Medium / 2 Low / 1 Nit findings remain out of scope for this gate. Tracked under bd-3qe but no spec coverage in v2.
