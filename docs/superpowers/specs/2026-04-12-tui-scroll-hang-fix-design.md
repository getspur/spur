# TUI Mouse Scroll Hang Fix

## Problem

When content in the SessionDetailView overflows the screen and the user scrolls up with the mouse, the entire TUI program hangs — no navigation, no keyboard input, complete freeze until the scroll gesture stops and the event queue drains.

## Root Cause

The event loop in `crates/spur-tui/src/app.rs:run_tui()` processes **one crossterm event per iteration**, then renders. Mouse scroll on macOS trackpad generates 30-60 events per gesture. Each event triggers a full render of all content with `Paragraph::wrap` (O(total_lines)). This creates a livelock: events queue faster than they drain, keyboard input is buried behind mouse events in the same stream.

**Why scroll UP specifically:** Scroll down while `is_following=true` is near-no-op (pinned to bottom). Scroll up sets `is_following=false`, forcing ratatui to compute wrap offsets from line 0 through entire content — the expensive render path.

## Design

### Event Batching (Primary Fix)

Change the event loop from:
```
wait_one_event → handle → render → repeat
```

To:
```
wait_one_event → handle → drain_all_remaining → handle_each → render_once
```

#### Implementation in `run_tui()`

```rust
loop {
    // Phase 1: Wait for at least one event (async yield point)
    tokio::select! {
        Some(Ok(ev)) = event_stream.next() => {
            app.handle_crossterm_event(ev);
        }
        result = event_rx.recv() => {
            match result {
                Ok(spur_event) => app.handle_spur_event(spur_event),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => { app.should_quit = true; }
            }
        }
        _ = tick_interval.tick() => {
            app.tick();
        }
    }

    // Phase 2: Non-blocking drain of remaining crossterm events
    loop {
        match tokio::time::timeout(Duration::ZERO, event_stream.next()).await {
            Ok(Some(Ok(ev))) => app.handle_crossterm_event(ev),
            _ => break,
        }
    }

    // Phase 3: Non-blocking drain of remaining spur events
    loop {
        match event_rx.try_recv() {
            Ok(spur_event) => app.handle_spur_event(spur_event),
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }

    // Phase 4: Single render
    if app.dirty {
        terminal.draw(|f| app.render(f))?;
        app.dirty = false;
    }

    if app.should_quit {
        break;
    }
}
```

### Scroll Coalescing (Polish)

Instead of calling `scroll_up()` N times per batched events, accumulate a net scroll delta in `handle_mouse_event` and expose a `scroll_by(delta: i32)` method on `ReactTrace`.

#### Changes to `ReactTrace`

Add two methods:

```rust
pub fn scroll_up_by(&mut self, lines: usize) {
    self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    self.is_following = false;
}

pub fn scroll_down_by(&mut self, lines: usize) {
    let max = self.max_offset();
    self.scroll_offset = self.scroll_offset.saturating_add(lines).min(max);
    if self.scroll_offset >= max {
        self.is_following = true;
    }
}
```

#### Changes to `App::handle_mouse_event`

Replace the `for _ in 0..SCROLL_LINES` loop with direct delta:

```rust
fn handle_mouse_event(&mut self, event: MouseEvent) {
    let delta: i32 = match event.kind {
        MouseEventKind::ScrollUp => -(SCROLL_LINES as i32),
        MouseEventKind::ScrollDown => SCROLL_LINES as i32,
        _ => return,
    };
    match self.current_view {
        ViewId::Dashboard => {
            if delta < 0 {
                self.dashboard.scroll_activity_up_by((-delta) as usize);
            } else {
                self.dashboard.scroll_activity_down_by(delta as usize);
            }
        }
        ViewId::SessionDetail(_) => {
            if let Some(ref mut detail) = self.session_detail {
                if delta < 0 {
                    detail.scroll_up_by((-delta) as usize);
                } else {
                    detail.scroll_down_by(delta as usize);
                }
            }
        }
    }
    self.dirty = true;
}
```

### Files to Modify

| File | Change |
|------|--------|
| `crates/spur-tui/src/app.rs` | Event loop batching in `run_tui()`, `handle_mouse_event` uses delta |
| `crates/spur-tui/src/components/react_trace.rs` | Add `scroll_up_by()`, `scroll_down_by()` |
| `crates/spur-tui/src/views/session_detail.rs` | Add `scroll_up_by()`, `scroll_down_by()` delegating to react_trace |
| `crates/spur-tui/src/views/dashboard.rs` | Add `scroll_activity_up_by()`, `scroll_activity_down_by()` |

### What This Does NOT Include

- **Virtualized rendering** — deferred unless profiling shows render is still slow after batching
- **Stale `max_offset()` fix** — secondary UX bug (premature `is_following` reactivation during streaming), separate ticket
- **Render frame throttle** — batching alone eliminates the need; add only if profiling shows >60fps renders

## Success Criteria

- Mouse scroll up/down on SessionDetailView with overflowed content remains responsive
- Keyboard input (Esc, j/k, typing) is never blocked by mouse scroll events
- No regression in scroll smoothness or auto-follow behavior
