# Tier Revamp Plan C — Tier 2: TUI Capability-Tease Modal

## Status: ✅ SHIPPED 2026-04-28

> Tasks 1–3 landed as commits f5cf3a87 / af0ae021 / 13fb4740
> (later rebased onto current main as 620a474d / 0efffeaa /
> c0f59d25). The dual-final-review findings (codex 🔴 / gemini 🟡)
> were integrated in a single cleanup commit on top of the rebase.
> See **Post-merge addendum (2026-04-28)** at the bottom of this
> document for the full landed-deviations table, the
> `App::feature_gate` startup-snapshot freshness gap, and the
> pointer to Plan C M1 where live `update_state` wiring lands.

> **Status (original):** Open. Filed 2026-04-28 after L9-MCTS
> evaluation selected this as the highest-EV next move post-Tier-1.
>
> **For agentic workers:** Ships in 3 atomic implementation tasks;
> each delegated to a fresh worker and gated by a 2-reviewer panel
> (`gemini` + `claude-code`) before merge. The brain (orchestrator)
> judges reviewer output and decides accept / iterate.

**Goal:** Extend the structured upgrade-CTA conversion mechanism
(landed in Tier 1 as CLI stderr output) to the TUI surface where
users spend most of their interactive time. When a feature gate
denies inside a TUI session, render a centered modal overlay
matching the existing modal idiom (QuitConfirm / CollisionModal /
HelpOverlay / PaletteOverlay) — typed-error name, current vs.
required tier, recovery affordances (`spur auth status` /
`spur auth login`), action keys.

Folded in: the **`SPUR_FORCE_TTY` test hook** filed as a Tier 1
follow-up (`docs/superpowers/plans/2026-04-28-tier-revamp-tier1-followup-tty-test-hook.md`).
Tier 2 closes that testability gap by wiring the env-var override
+ strengthening the binary smoke to assert CTA shape end-to-end.

**Why this is the next move (per L9-MCTS verdict):**

1. **Highest conversion-EV:** TUI is the dominant interactive
   surface; Tier 1 only closed CLI denials. Most users hit gates
   *inside* the TUI; today they get a useless legacy error and
   bounce.
2. **Best foundation-validation timing:** Tier 1's
   `format_upgrade_cta -> String` API has 1 caller. Adding a TUI
   renderer alongside lets us refine the contract under realistic
   pressure with cheap rollback.
3. **Compounds on Plan D (trial JWT):** Trial CTA needs a TUI
   surface to render on. Tier 2 first → Plan D ships into a wired
   surface from day 1.
4. **Bundles `SPUR_FORCE_TTY` hook** (closes Tier 1 follow-up).
5. **Strictly idiomatic:** spur-tui has 4 existing modal precedents.
   Adding a 5th = zero new infrastructure.

**Tech Stack:** Rust 2021, ratatui (existing dep in spur-tui),
anyhow, std `IsTerminal`, `assert_cmd`. No new deps.

---

## Spec grounding

- **Tier 1 plan + post-merge addendum:**
  `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier1-cli-denial-cta.md`.
  In particular the addendum's "Foundation API stable for Tier 2 /
  Tier 3" section — Tier 2 builds on the public
  `spur_license::upgrade_cta::{find_gate_error, format_upgrade_cta}`
  API without breaking it.
- **Tier 1 follow-up to fold in:**
  `docs/superpowers/plans/2026-04-28-tier-revamp-tier1-followup-tty-test-hook.md`
  (SPUR_FORCE_TTY env hook).
- **Existing spur-tui modal patterns** (precedent inventory):
  - `crates/spur-tui/src/components/quit_confirm.rs:14-19` — 56×10
    centered Rect, `Clear` + Yellow-bordered `Block` + `Paragraph`.
  - `crates/spur-tui/src/components/collision_modal.rs:17-22` —
    70×14 data-bearing variant (closest precedent for Tier 2).
  - `crates/spur-tui/src/components/help_overlay.rs:12-27` — 66×50
    Cyan-bordered.
  - `crates/spur-tui/src/components/palette_overlay.rs:200-217` —
    `impl Widget` directly + private `modal_rect()` helper.
- **Event-loop priority chain:** `crates/spur-tui/src/app.rs::handle_crossterm_event`
  lines 832–1057. Sequential `return`-on-consume, no stack. Insert
  Tier 2 modal check between `collision_modal` (line ~854) and
  `help_visible` (line ~893) — denial demands user attention so it
  preempts informational overlays, but defers to Quit/Collision.
- **App slot pattern:** `App::collision_modal: Option<CollisionModalState>`
  (lines ~204) — Tier 2 mirrors with `App::upgrade_modal:
  Option<UpgradeModalState>`.
- **Action enum location:** `crates/spur-tui/src/action.rs` — Tier 2
  adds `Action::ShowUpgradeModal { err, required_tier }`.
- **Tonal palette:** `crates/spur-tui/src/components/status_bar.rs::LicenseBadge`
  (lines 36–48) defines `Neutral=DarkGray, Success=Green+BOLD,
  Warning=Yellow+BOLD, Danger=Red+BOLD`. The denial CTA is "Warning"
  semantically (Yellow border + title), with Green-BOLD for
  affirmative recovery commands.

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-license/src/upgrade_cta.rs` | Modify | Pure-additive: add `required_tier_for(key) -> Option<Plan>` helper. Existing `format_upgrade_cta` / `find_gate_error` UNCHANGED (Tier 1 contract preserved). |
| `crates/spur-license/src/gate.rs` | Modify | Ensure `FeatureGateError` is `Clone` (add `#[derive(Clone)]` if missing — App state needs ownership). |
| `crates/spur-tui/src/components/upgrade_modal.rs` | Create | New modal component. `render(frame, area, &UpgradeModalState)` + private `modal_lines()` styled-Line builder. |
| `crates/spur-tui/src/components/mod.rs` | Modify | `pub mod upgrade_modal;` |
| `crates/spur-tui/src/action.rs` | Modify | Add `Action::ShowUpgradeModal { err: FeatureGateError, required_tier: Option<Plan> }`. |
| `crates/spur-tui/src/app.rs` | Modify | Add `App::upgrade_modal: Option<UpgradeModalState>` slot, event-priority handler, `process_action` arm, render call, MVP gate-check site for `cli_core_exec`. |
| `crates/spur-cli/src/main.rs` | Modify | Add `is_tty_or_forced()` helper for SPUR_FORCE_TTY env override (debug-only `cfg`). Replace direct `is_terminal()` call in `render_top_level_error`. |
| `crates/spur-cli/tests/cli_core_gate_e2e.rs` | Modify | Add new test `spur_exec_under_stripped_key_renders_full_cta_under_force_tty` asserting CTA shape end-to-end. Existing test unchanged. |

No new deps. `FeatureGateError`'s `Clone` derive is the only API
surface delta in `spur-license` outside the new helper fn.

---

## Task 1: spur-license API extension (`required_tier_for` + Clone)

**Worker assignment:** claude-code (implementer). Reviewers:
gemini, claude-code (2 gates in parallel).

**Files:**
- Modify: `crates/spur-license/src/upgrade_cta.rs`
- Modify: `crates/spur-license/src/gate.rs`
- Modify: `crates/spur-license/src/lib.rs` (re-export `Plan` if needed)

### Subtask 1a: Verify and add `Clone` to `FeatureGateError`

In `crates/spur-license/src/gate.rs`, locate the `FeatureGateError`
definition. It is currently `#[non_exhaustive]` and derives at
minimum `Debug` + `thiserror::Error`. Tier 2's TUI App needs to
**own** the error inside `App::upgrade_modal: Option<UpgradeModalState>`,
so add `Clone`:

```rust
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum FeatureGateError {
    #[error("feature `{key}` is not available on tier `{tier}`")]
    Denied { key: FeatureKey, tier: Tier },
}
```

`FeatureKey` and `Tier` should already be `Clone` + `Copy`. If
not, add the derives.

**Why this is safe:** `Clone` is purely additive; no existing
caller breaks. The error is small (two enum-discriminant values),
so cloning is cheap.

### Subtask 1b: Add `required_tier_for` helper

In `crates/spur-license/src/upgrade_cta.rs`, append a new function:

```rust
use crate::Plan;

/// Return the lowest tier that grants `key`, walking the embedded
/// policy in ascending order (Community → Pro → Team → Enterprise).
/// Returns `None` if no tier grants the key (e.g. unknown / future
/// key not yet in policy).
///
/// The TUI upgrade-CTA modal uses this to surface "Required tier:
/// Pro" alongside "Current tier: Community" — the single most
/// conversion-relevant line in the modal.
///
/// Note: this is intentionally separate from `FeatureGate` itself,
/// which is per-instance (snapshot of resolved features for the
/// current license state). The required-tier query needs the
/// global policy, not a snapshot.
pub fn required_tier_for(key: FeatureKey) -> Option<Plan> {
    use crate::policy::resolve_features;
    for plan in [Plan::Community, Plan::Pro, Plan::Team, Plan::Enterprise] {
        let tier_label = plan_to_resolver_label(plan);
        if resolve_features(tier_label).contains(&key) {
            return Some(plan);
        }
    }
    None
}

fn plan_to_resolver_label(plan: Plan) -> &'static str {
    match plan {
        Plan::Community => "community",
        Plan::Pro => "pro",
        Plan::Team => "team",
        Plan::Enterprise => "enterprise",
        // LTD plans inherit Pro feature set; treat as Pro for the
        // required-tier display.
        Plan::StarterLtd | Plan::BuilderLtd | Plan::FounderLtd => "pro",
        Plan::Unknown => "community",
    }
}
```

**Implementer note:** the implementer should survey
`crates/spur-license/src/policy.rs` (or wherever `resolve_features`
actually lives — Tier 1's plan referenced it but the canonical path
should be verified) and adjust the import + signature to match. The
shape above is the contract; the implementation may differ in
imports.

### Subtask 1c: Unit tests for `required_tier_for`

Add to the existing `crates/spur-license/src/upgrade_cta.rs::tests`
module:

```rust
#[test]
fn required_tier_for_community_only_key_returns_community() {
    // Pick a key that is in community per embedded policy.
    let plan = required_tier_for(FeatureKey::CLI_CORE_INIT);
    assert_eq!(plan, Some(Plan::Community));
}

#[test]
fn required_tier_for_pro_only_key_returns_pro() {
    // Pick a key that is in pro but NOT in community per embedded policy.
    // Implementer must verify the chosen key has this property.
    let plan = required_tier_for(FeatureKey::CLI_CORE_EXEC);
    assert_eq!(plan, Some(Plan::Pro));
}

#[test]
fn required_tier_for_unknown_key_returns_none() {
    // FeatureKey is #[non_exhaustive] but if a future test uses a
    // key not present in any tier, this should return None. Construct
    // such a case using FeatureKey::from_known with a fake string,
    // or skip if the type doesn't support it. Implementer's choice.
}
```

**Implementer note:** the exact `FeatureKey` constants used in
asserts must be verified against the current embedded policy
(`crates/spur-license/resources/default_policy.json`). If
`CLI_CORE_INIT` is now Pro-tier and `CLI_CORE_EXEC` is Community,
swap them. The test SHAPE is the contract.

### Acceptance for Task 1

- [ ] `FeatureGateError: Clone` (verified by `let _: FeatureGateError = err.clone();` in tests)
- [ ] `required_tier_for(key) -> Option<Plan>` implemented
- [ ] 2-3 unit tests covering community-only key, pro-only key, edge case
- [ ] Existing 5 unit tests in `upgrade_cta.rs::tests` still pass
- [ ] `format_upgrade_cta` byte-identical (no regression in the
      Tier 1 unit tests asserting "spur auth status" / "spur auth
      login --key" / "spur auth logout" presence)
- [ ] Workspace builds clean: `scripts/spur-cargo build -p spur-license`
- [ ] No clippy warnings: `scripts/spur-cargo clippy -p spur-license -- -D warnings`
- [ ] No fmt diff: `scripts/spur-cargo fmt -p spur-license -- --check`

---

## Task 2: spur-tui modal widget + dispatch wiring + MVP gate site

**Worker assignment:** claude-code (implementer). Reviewers:
gemini, claude-code (2 gates in parallel).

**Files:**
- Create: `crates/spur-tui/src/components/upgrade_modal.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`

### Subtask 2a: Create the modal component

Create `crates/spur-tui/src/components/upgrade_modal.rs`:

```rust
//! Plan C Tier 2 — TUI capability-tease modal. Renders a
//! centered overlay when a feature gate denies a TUI-side action,
//! converting denial-without-recovery into structured upgrade
//! pressure (the same conversion mechanism Tier 1 wired into CLI
//! stderr).
//!
//! Pattern: matches CollisionModal — data-bearing render fn
//! (`render(frame, area, &UpgradeModalState)`) + Yellow-bordered
//! Block + Clear + styled Paragraph.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use spur_license::{FeatureGateError, Plan};

/// Data carried by `App::upgrade_modal` while the modal is visible.
#[derive(Debug, Clone)]
pub struct UpgradeModalState {
    pub err: FeatureGateError,
    pub required_tier: Option<Plan>,
}

const MODAL_WIDTH: u16 = 70;
const MODAL_HEIGHT: u16 = 16;

pub fn render(frame: &mut Frame, area: Rect, state: &UpgradeModalState) {
    let popup = centered_rect(area, MODAL_WIDTH, MODAL_HEIGHT);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(
            " Feature unavailable ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let lines = modal_lines(state);
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup);
}

fn modal_lines(state: &UpgradeModalState) -> Vec<Line<'static>> {
    let FeatureGateError::Denied { key, tier } = &state.err;
    let mut out: Vec<Line<'static>> = Vec::with_capacity(14);

    // Spacer
    out.push(Line::from(""));

    // Feature: <key>
    out.push(Line::from(vec![
        Span::raw("  Feature: "),
        Span::styled(
            key.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Current tier: <tier>  (DarkGray for community, neutral otherwise)
    out.push(Line::from(vec![
        Span::raw("  Current tier: "),
        Span::styled(
            tier.to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    // Required tier: <plan>  (Cyan-BOLD if Some, omit row if None)
    if let Some(req) = state.required_tier {
        out.push(Line::from(vec![
            Span::raw("  Required tier: "),
            Span::styled(
                format!("{req:?}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Spacer + "To unlock this feature:"
    out.push(Line::from(""));
    out.push(Line::from("  To unlock this feature:"));

    // Recovery affordances
    out.push(Line::from(vec![
        Span::raw("    \u{2022} View tier comparison:  "),
        Span::styled(
            "spur auth status",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    out.push(Line::from(vec![
        Span::raw("    \u{2022} Activate a license:    "),
        Span::styled(
            "spur auth login --key <KEY>",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Spacer + footnote (DarkGray)
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        "  Already have a license? Run `spur auth logout` then re-login",
        Style::default().fg(Color::DarkGray),
    )));
    out.push(Line::from(Span::styled(
        "  to refresh.",
        Style::default().fg(Color::DarkGray),
    )));

    // Spacer + action keys
    out.push(Line::from(""));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "[Esc/q]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Dismiss   "),
        Span::styled(
            "[s]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Status   "),
        Span::styled(
            "[l]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Login"),
    ]));

    out
}

fn centered_rect(outer: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(outer.width);
    let h = height.min(outer.height);
    let x = outer.x + (outer.width.saturating_sub(w)) / 2;
    let y = outer.y + (outer.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w, height: h }
}
```

**Implementer note on tier display:** `Plan` does not have a
`Display` impl in spur-license today (it's `#[derive(Debug)]` only).
Using `format!("{req:?}")` produces e.g. "Pro" / "Community" which
matches the desired output. If the implementer prefers, add a
`Display` impl in `crates/spur-license/src/lib.rs` (1 LOC) and use
`{req}` instead. Either is acceptable; the `Debug` shortcut is
chosen here to avoid a second API churn.

Similarly `Tier`: use existing `Display` impl (it has one — see
the `FeatureGateError::Denied` `#[error]` format string referencing
`{tier}`).

### Subtask 2b: Wire the action variant

In `crates/spur-tui/src/action.rs`, add to the `Action` enum:

```rust
ShowUpgradeModal {
    err: spur_license::FeatureGateError,
    required_tier: Option<spur_license::Plan>,
},
```

### Subtask 2c: Wire the App slot + event handler + render

In `crates/spur-tui/src/app.rs`:

1. Add field to `App` struct (alongside `collision_modal`):
   ```rust
   upgrade_modal: Option<crate::components::upgrade_modal::UpgradeModalState>,
   ```
   Initialize to `None` in the constructor.

2. Add `process_action` arm:
   ```rust
   Action::ShowUpgradeModal { err, required_tier } => {
       self.upgrade_modal = Some(
           crate::components::upgrade_modal::UpgradeModalState { err, required_tier }
       );
   }
   ```

3. Insert event-priority check in `handle_crossterm_event`,
   between the `collision_modal` branch (line ~854) and the
   `help_visible` branch (line ~893):
   ```rust
   if self.upgrade_modal.is_some() {
       match key.code {
           KeyCode::Esc | KeyCode::Char('q') => {
               self.upgrade_modal = None;
           }
           KeyCode::Char('s') => {
               self.upgrade_modal = None;
               self.show_user_warning(
                   "Run `spur auth status` to view tiers and license state.".into()
               );
           }
           KeyCode::Char('l') => {
               self.upgrade_modal = None;
               self.show_user_warning(
                   "Run `spur auth login --key <KEY>` to activate a license.".into()
               );
           }
           _ => { /* swallow other keys while modal is up */ }
       }
       return;
   }
   ```

4. Add render call in the main render fn (alongside other modal
   render calls):
   ```rust
   if let Some(state) = &self.upgrade_modal {
       crate::components::upgrade_modal::render(frame, frame.area(), state);
   }
   ```
   Render order: this goes AFTER all other overlays so the modal
   draws on top. (If event priority gives us preemption over
   `help_visible`, render order should match.)

### Subtask 2d: Wire the MVP gate-check site

The interactive command-execution path inside the TUI is the
chosen MVP site (see "L9-MCTS UI/UX evaluation" in this plan's
parent message). Find the spur-tui handler that invokes the
brain's `exec` flow on user input, OR — if that path doesn't exist
in spur-tui directly — wire the gate at the TUI's brain-start path
(`Action::BrainStart` or analogous) using the `cli_core_brain_start`
key if it exists, falling back to `cli_core_exec` if not.

**Implementer authority:** the implementer surveys
`crates/spur-tui/src/app.rs` and `crates/spur-tui/src/action.rs`
to identify the cleanest single site. Pick ONE site for MVP.
Document the choice in a comment.

Sketch (replace `XYZ` with the actual handler):
```rust
fn handle_xyz(&mut self) -> Result<(), ()> {
    let gate = self.license.feature_gate();
    if let Err(err) = spur_license::require_feature(
        &gate,
        spur_license::FeatureKey::CLI_CORE_EXEC,
    ) {
        self.process_action(Action::ShowUpgradeModal {
            err,
            required_tier: spur_license::upgrade_cta::required_tier_for(
                spur_license::FeatureKey::CLI_CORE_EXEC,
            ),
        });
        return Err(());
    }
    // ... existing handler logic ...
    Ok(())
}
```

**De-dup policy (per L9-MCTS):** none. Re-pop on every denial.
Adding session-level `HashSet<FeatureKey>` de-dup is YAGNI for
MVP; defer if it becomes annoying in practice.

### Subtask 2e: Component-level test

Add `#[cfg(test)] mod tests` in `upgrade_modal.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spur_license::{FeatureKey, Tier};

    fn fixture_state(required: Option<Plan>) -> UpgradeModalState {
        UpgradeModalState {
            err: FeatureGateError::Denied {
                key: FeatureKey::CLI_CORE_EXEC,
                tier: Tier::Community,
            },
            required_tier: required,
        }
    }

    #[test]
    fn modal_lines_includes_key_name() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        assert!(flat.contains("cli_core_exec"), "lines must name key: {flat}");
    }

    #[test]
    fn modal_lines_includes_recovery_commands() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        assert!(flat.contains("spur auth status"));
        assert!(flat.contains("spur auth login"));
        assert!(flat.contains("spur auth logout"));
    }

    #[test]
    fn modal_lines_includes_required_tier_when_some() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        assert!(flat.contains("Required tier"));
        assert!(flat.contains("Pro"));
    }

    #[test]
    fn modal_lines_omits_required_tier_when_none() {
        let lines = modal_lines(&fixture_state(None));
        let flat = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        assert!(!flat.contains("Required tier"));
    }

    #[test]
    fn modal_lines_includes_action_keys() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        assert!(flat.contains("[Esc/q]"));
        assert!(flat.contains("[s]"));
        assert!(flat.contains("[l]"));
    }

    #[test]
    fn centered_rect_clamps_to_outer_when_smaller() {
        let outer = Rect { x: 0, y: 0, width: 40, height: 10 };
        let r = centered_rect(outer, 70, 16);
        assert_eq!(r.width, 40);
        assert_eq!(r.height, 10);
    }
}
```

### Acceptance for Task 2

- [ ] `crates/spur-tui/src/components/upgrade_modal.rs` exists with
      `pub struct UpgradeModalState`, `pub fn render(...)`, private
      `modal_lines(...)` + `centered_rect(...)`
- [ ] `Action::ShowUpgradeModal { err, required_tier }` variant
- [ ] `App::upgrade_modal: Option<UpgradeModalState>` slot
- [ ] Event handler captures `Esc/q` / `s` / `l` while modal is up;
      other keys swallowed
- [ ] `process_action` arm sets the modal slot
- [ ] Render call in main render fn (drawn last so it's on top)
- [ ] At least ONE TUI handler gates on `cli_core_exec` (or
      similar) and emits `Action::ShowUpgradeModal` on denial
- [ ] 6+ component tests covering: key name, recovery commands,
      required-tier present/absent, action keys, centered_rect clamp
- [ ] Workspace builds clean: `scripts/spur-cargo build -p spur-tui`
- [ ] No clippy warnings: `scripts/spur-cargo clippy -p spur-tui --tests -- -D warnings`
- [ ] No fmt diff

---

## Task 3: SPUR_FORCE_TTY hook + binary-smoke shape assertion

**Worker assignment:** claude-code (implementer). Reviewers:
gemini, claude-code (2 gates in parallel).

**Files:**
- Modify: `crates/spur-cli/src/main.rs` (add `is_tty_or_forced` helper)
- Modify: `crates/spur-cli/tests/cli_core_gate_e2e.rs` (add new test)

### Subtask 3a: Add the env-override helper

In `crates/spur-cli/src/main.rs`, locate `render_top_level_error`
(currently calls `std::io::stderr().is_terminal()` directly) and
extract the TTY check:

```rust
/// Render the top-level error. If stderr is a TTY (or
/// `SPUR_FORCE_TTY=1` in debug builds) and the error chain
/// contains a `FeatureGateError`, render the structured upgrade
/// CTA. Otherwise fall through to anyhow's Display chain
/// (`{:#}`).
fn render_top_level_error(err: &anyhow::Error) {
    if is_tty_or_forced() {
        if let Some(gate_err) = spur_license::upgrade_cta::find_gate_error(err) {
            eprint!("{}", spur_license::upgrade_cta::format_upgrade_cta(gate_err));
            return;
        }
    }
    eprintln!("Error: {err:#}");
}

fn is_tty_or_forced() -> bool {
    if std::io::stderr().is_terminal() {
        return true;
    }
    // Debug-only override for assert_cmd-based binary tests, which
    // do not allocate a pty for spawned children. Gated on
    // `cfg(debug_assertions)` so it cannot leak into release.
    #[cfg(debug_assertions)]
    {
        if std::env::var("SPUR_FORCE_TTY").is_ok() {
            return true;
        }
    }
    false
}
```

### Subtask 3b: Strengthen the binary smoke

In `crates/spur-cli/tests/cli_core_gate_e2e.rs`, ADD (do not
replace) a new test alongside the existing
`spur_exec_under_stripped_key_renders_typed_error_at_binary_boundary`:

```rust
#[test]
fn spur_exec_under_stripped_key_renders_full_cta_under_force_tty() {
    // Plan C Tier 2 — closes the Tier 1 follow-up
    // (`2026-04-28-tier-revamp-tier1-followup-tty-test-hook.md`).
    //
    // assert_cmd does not allocate a pty for the child, so
    // `is_terminal()` returns false and the CTA path is normally
    // bypassed. The debug-only `SPUR_FORCE_TTY=1` env var forces
    // the TTY-gate to true, exercising the CTA renderer dispatch
    // path end-to-end at the binary boundary.
    //
    // Together with the existing
    // `spur_exec_under_stripped_key_renders_typed_error_at_binary_boundary`
    // smoke (which only asserts key-name propagation without the
    // CTA), this gives a regression net for:
    //   1. `is_terminal()` predicate inversion (e.g. `if !...`)
    //   2. dropping the `find_gate_error` branch
    //   3. renaming `format_upgrade_cta` without updating main.rs
    //
    // All three would PASS the existing key-name-only smoke but
    // FAIL this CTA-shape smoke.
    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .env("SPUR_LICENSE_TEST_STRIP_KEYS", "cli_core_exec")
        .env("SPUR_FORCE_TTY", "1")
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .args(["exec", "--agent", "claude-code", "irrelevant-task"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("cli_core_exec"),
        "stderr must name the denied key, got:\n{stderr}",
    );
    assert!(
        stderr.contains("spur auth status"),
        "stderr must include `spur auth status` recovery line, got:\n{stderr}",
    );
    assert!(
        stderr.contains("spur auth login --key"),
        "stderr must include `spur auth login --key` recovery line, got:\n{stderr}",
    );
    assert!(
        stderr.contains("spur auth logout"),
        "stderr must include `spur auth logout` re-login hint, got:\n{stderr}",
    );
}
```

### Acceptance for Task 3

- [ ] `is_tty_or_forced()` helper exists, gated `cfg(debug_assertions)`
      around the `SPUR_FORCE_TTY` env check
- [ ] `render_top_level_error` calls `is_tty_or_forced()` (not
      `is_terminal` directly)
- [ ] Release builds (`cargo build --release -p spur-cli`) do NOT
      compile in the env-var override — verify by inspecting the
      generated assembly OR by adding a tiny `#[cfg(not(debug_assertions))]`
      compile-time assertion that `SPUR_FORCE_TTY` literal is absent
      (skip if too costly)
- [ ] New test
      `spur_exec_under_stripped_key_renders_full_cta_under_force_tty`
      passes
- [ ] Existing test
      `spur_exec_under_stripped_key_renders_typed_error_at_binary_boundary`
      still passes (no regression)
- [ ] Workspace builds clean
- [ ] No clippy warnings; no fmt diff
- [ ] Update
      `docs/superpowers/plans/2026-04-28-tier-revamp-tier1-followup-tty-test-hook.md`
      to mark the follow-up resolved (status header → `✅ RESOLVED
      by Tier 2 Task 3, commit <SHA>`)

---

## Final sweep (judge-only, not delegated)

After Tasks 1, 2, 3 all pass their 2-gate review and merge:

- [ ] `scripts/spur-cargo build --workspace`
- [ ] `scripts/spur-cargo test -p spur-license -p spur-tui -p spur-cli`
- [ ] `scripts/spur-cargo clippy -p spur-license -p spur-tui -p spur-cli --tests -- -D warnings`
- [ ] `scripts/spur-cargo fmt --all -- --check`
- [ ] **Manual TUI verification** under
      `SPUR_LICENSE_TEST_STRIP_KEYS=cli_core_exec`:
      run `cargo run -p spur-cli -- tui --brain claude-code` and
      trigger the gated TUI path; verify modal renders correctly
      (key name, current tier, required tier "Pro", recovery
      commands, action keys). Press `Esc`, `q`, `s`, `l` and
      verify each does the right thing.
- [ ] Update
      `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier1-cli-denial-cta.md`'s
      post-merge addendum to add a "Tier 2 has now landed" pointer
      (1 line at end of addendum).
- [ ] Total commits expected: 4-5 (Task 1, Task 2, Task 3, possibly
      Cargo.lock + follow-up doc resolution)

## Acceptance criteria for Tier 2 as a whole

- [ ] Every TUI-mode `FeatureGateError` denial routed through the
      MVP gate-check site renders the upgrade modal
- [ ] Modal matches existing overlay idiom (Yellow Border + Clear +
      centered Rect, ~70×16)
- [ ] Tier 1 CLI behavior is byte-identical (no regression in
      `format_upgrade_cta` output, no regression in
      `spur_exec_under_stripped_key_renders_typed_error_at_binary_boundary`)
- [ ] `SPUR_FORCE_TTY=1` debug-only env override exercises the CTA
      path end-to-end under `assert_cmd`
- [ ] New binary smoke asserts CTA SHAPE (not just key-name)
- [ ] Foundation API (`spur_license::upgrade_cta::*`) gains
      `required_tier_for(key)` without breaking existing callers
- [ ] `FeatureGateError: Clone` derive added (App-state ownership)
- [ ] No new deps in any crate
- [ ] Existing 5 unit tests in `upgrade_cta.rs::tests` still pass
- [ ] At least 6 new component tests cover the modal renderer

## Out of scope for Tier 2 (deferred)

- **Per-key user-facing labels** (e.g. translating `cli_core_exec`
  → "the `spur exec` subcommand"). Tier 2 polish or Tier 3 work —
  needs a registry table or `FeatureKey::user_facing_label()`
  method. Modal renders the raw key for now.
- **Tier-aware copy branching** (different recovery copy for
  Community vs tampered-Pro vs trial-expired). Tier 3 / Plan D
  work; would refine `RecoveryLine` enum (yet to be introduced).
- **Trial JWT CTA content** (e.g. "[t] Start 14-day trial"). Plan
  D / Tier 3 — depends on trial JWT machinery + spec.
- **Subprocess launch on `[s]` / `[l]`** action keys. Modal merely
  WRITES the recovery hint to the status bar. No clipboard infra,
  no subprocess spawn. YAGNI.
- **Modal de-dup / suppression policy.** Re-pops on every denial
  for MVP. If denial spam becomes a UX problem, add session-level
  `HashSet<FeatureKey>` filter in a follow-up.
- **Tier-color palette helper** (`tier_color(Plan) -> Color`).
  Inline for MVP. If Tier 3 needs the same colors elsewhere,
  extract then.
- **Plan C M1 (TUI/ACP gate spread).** Tier 2 wires ONE gate site;
  M1 is the dedicated wave for spreading gates throughout the TUI
  and ACP layer. Orthogonal.
- **Real Team / Enterprise feature delta.** Currently both inherit
  the Pro feature set in the embedded policy; needs product authority
  to define what differentiates them.
- **Resolver `@inherit:pro` directive** (refactor to DRY the
  inlined Pro features in team / enterprise blocks). Pure cleanup;
  doesn't gate Tier 2.

## Self-review (pre-dispatch checklist)

- [x] Spec coverage: every gate-fire site uses `?` to bubble
      `FeatureGateError` via anyhow's `From` impl, so a chain-walk
      lookup catches them — but Tier 2 instead emits an `Action`
      directly at the gate-check site, avoiding anyhow context
      stripping. The CLI path keeps the chain-walk for backward
      compat.
- [x] No new deps (ratatui already in spur-tui)
- [x] Test isolation: SPUR_LICENSE_TEST_STRIP_KEYS fixture pattern
      reused; new test additionally uses SPUR_FORCE_TTY (debug-only)
- [x] No URLs (avoids product-authority gap)
- [x] No per-key labels (defers Tier 3 dep)
- [x] No subprocess infra on action keys (YAGNI)
- [x] FeatureGateError Clone derive is purely additive
- [x] Tier 1 CLI byte-identical (regression-asserted by existing
      5 unit tests + existing binary smoke)
- [x] SPUR_FORCE_TTY hook is `cfg(debug_assertions)`-gated; cannot
      leak into release
- [x] Modal pattern duplicates 4 existing precedents (zero new
      infra)
- [x] Event-priority insertion point (between collision and help)
      preserves Quit > Collision > Upgrade > Help > Palette
      ordering — denial demands attention but defers to user's
      desire to leave

---

## Note on FeatureGateError ownership

The TUI app needs to OWN a `FeatureGateError` inside
`App::upgrade_modal` (not a borrow). Existing Tier 1 callers
work with `Option<&FeatureGateError>` from `find_gate_error`.
After Task 1 adds `Clone`, both patterns compose: TUI gate-check
sites own a fresh `FeatureGateError` returned by `require_feature`
directly (no chain-walk needed at the call site, since the gate
check is the immediate boundary).

If a future surface receives a `&FeatureGateError` via chain-walk
(e.g. a hypothetical async error pipeline), it can `.clone()` to
own.

## Note on `Plan::Display`

`Plan` is `#[derive(Debug)]`-only today. The modal uses
`format!("{plan:?}")` to render the tier name in the "Required
tier:" row. Acceptable for MVP; the Debug output is a single PascalCase
identifier which is the desired display form. Adding a `Display`
impl (1 LOC) is a clean follow-up but not required.

## References

- Tier 1 plan + post-merge addendum:
  `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier1-cli-denial-cta.md`
- Tier 1 follow-up (folded into Tier 2 Task 3):
  `docs/superpowers/plans/2026-04-28-tier-revamp-tier1-followup-tty-test-hook.md`
- Existing modal precedents:
  - `crates/spur-tui/src/components/quit_confirm.rs:14-19`
  - `crates/spur-tui/src/components/collision_modal.rs:17-22`
  - `crates/spur-tui/src/components/help_overlay.rs:12-27`
  - `crates/spur-tui/src/components/palette_overlay.rs:200-217`
- Event priority chain: `crates/spur-tui/src/app.rs::handle_crossterm_event` lines 832–1057
- LicenseBadge tonal palette: `crates/spur-tui/src/components/status_bar.rs:36-48`

## Post-merge addendum (2026-04-28)

**Landed shape vs. plan v1 prescription:**

| Concern | v1 prescription | v2 landed | Driver |
|---|---|---|---|
| `Action::ShowUpgradeModal` field name | `error: FeatureGateError` | `err: FeatureGateError` | Task 2 review (gemini): minor — match call-site brevity. |
| `App::feature_gate` initialization | Wire from `LicenseStateEvent` snapshot | Embedded `PolicyResolver::embedded()` snapshot | Task 2 review: keeps Tier 2 self-contained; live updates land in Plan C M1 (see *freshness gap* below). |
| Modal stacking when `collision_modal` visible | Render upgrade_modal on top | Suppress upgrade_modal render | Cross-Tier-2 review (codex 🔴): visual must match input precedence. |
| Quit chord handling with modal visible | Modal handler swallows non-recovery keys | `is_quit_chord` runs BEFORE upgrade_modal handler | Cross-Tier-2 review (codex 🔴): `Ctrl+C` / `Ctrl+Q` must always reach the global quit path; the modal's `_ => swallow` arm cannot eat quit chords. Preserves main's `is_quit_chord` semantics. |
| `Required tier` row when `required == current` | Always render when `Some(_)` | Omit when `Tier::from_plan(req) == current_tier` | Cross-Tier-2 review (codex 🔴): rendering "Required tier: Community" alongside "Current tier: Community" is confusing for the stripped-key demo path. |
| `Plan` rendering in modal | `format!("{plan:?}")` Debug-format | `Plan::label()` | Cross-Tier-2 review (codex 🟡): `Plan::label()` already exists in `spur-license/src/lib.rs` and is the canonical display path. |
| `SPUR_FORCE_TTY` env-var semantics | `is_ok()` (accepts empty) | `is_ok_and(|v| !v.is_empty())` | Task 3 codex review: empty `SPUR_FORCE_TTY=` should not force TTY. |
| Release-safety wording for cfg gate | "cannot leak into release builds" | "not present in default release builds (when `debug_assertions` is off)" | Task 3 codex review: precision — `RUSTFLAGS=-C debug-assertions=on` re-enables it. |
| Test name for CTA-shape smoke | `..._renders_full_cta_under_force_tty` | `..._renders_structured_upgrade_cta_under_force_tty` | Task 3 codex review: "full" overstates 4 substring asserts; "structured upgrade CTA" is precise. |
| `dirty = true` after `ShowUpgradeModal` action | implied | explicit `self.dirty = true;` | Task 2 review (gemini): explicit re-render trigger; documented improvement. |
| `[s]` / `[l]` action-key UX copy | `Run \`spur auth status\`` | `Run \`spur auth status\` in a shell to view tiers and license state.` | Task 2 review (gemini): "in a shell" disambiguates that the user must drop out of the TUI to invoke the command. Same copy expansion on `[l]`. |

**Out of scope for Tier 2, all honored:**
- ✅ No per-key user-facing labels (Tier 3 / future)
- ✅ No tier-aware copy branching (Tier 3)
- ✅ No JSON output for the CTA
- ✅ No Pro-only gate site (community-tier MVP demo path only)
- ✅ No session-level upgrade-modal de-dup (YAGNI for MVP)

**Freshness gap — `App::feature_gate` startup snapshot:**

The `App::feature_gate` field is initialized once at TUI startup
from `PolicyResolver::embedded()`. Live `LicenseStateEvent`
updates that arrive over the broadcast bus are NOT pumped into
`feature_gate.update_state(...)` today. The MVP gate site at
`Action::SendMessage` therefore reflects only the embedded policy
+ `SPUR_LICENSE_TEST_STRIP_KEYS` env override — sufficient for
the community-tier denial demo path, but a Pro-only gate site
added before live updates are wired would deny Pro users for the
session lifetime even after their license refreshes.

**This is the explicit blocker for adding any Pro-only TUI gate
site.** Before Tier 3 (or any other surface) lands a Pro-tier
gate-check inside `App`, the `update_state(...)` plumbing from
`LicenseStateEvent` must land first.

**Plan C M1 is where live `App::feature_gate.update_state(...)` wires up.**
That wave deals with the dispatch-bus → license runtime → TUI
gate refresh chain; Tier 2 deliberately stays in front of that
work since the freshness gap doesn't affect the MVP demo path.

**Cleanup commit applied on top of rebased Tasks 1–3:**

1. **Quit-chord pass-through** — `is_quit_chord(key)` runs above
   the `upgrade_modal` handler in `handle_crossterm_event` so
   `Ctrl+C` / `Ctrl+Q` always reach `request_quit()` regardless of
   modal visibility.
2. **Modal stacking suppression** — the upgrade_modal render block
   is gated on `self.collision_modal.is_none()`; when both modals
   would be visible, only the collision_modal renders, matching
   input precedence.
3. **Same-tier required_tier elision** — `modal_lines` skips the
   "Required tier:" row when `Tier::from_plan(required) ==
   current_tier`. Unit test
   `modal_lines_omits_required_tier_when_same_as_current` locks
   the behavior.
4. **`SPUR_FORCE_TTY` non-empty semantics + doc precision** —
   `is_ok_and(|v| !v.is_empty())` + new doc comment that scopes
   "release builds" to `debug_assertions = off` (default `cargo
   build --release`).
5. **Test rename** — `..._renders_structured_upgrade_cta_under_force_tty`.
6. **`Plan::label()` usage** — replaces `format!("{plan:?}")` in
   `modal_lines`.

**Foundation API stable for Tier 3:**

```rust
// TUI gate-check site pattern (post-Tier-2):
if let Err(err) = spur_license::require_feature(&self.feature_gate, FeatureKey::FOO) {
    let required_tier = spur_license::upgrade_cta::required_tier_for(FeatureKey::FOO);
    self.process_action(Action::ShowUpgradeModal { err, required_tier });
    return;
}
// Modal handles Esc/q dismiss + s/l shell hint + visual stacking + quit-chord
// pass-through automatically. Required-tier row hides when same as current.
```

Plan doc preserved as audit trail above; this addendum is the
canonical reference for Tier 3 implementers and for whoever wires
`App::feature_gate.update_state(...)` in Plan C M1.
