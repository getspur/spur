# Jute Notebook Tabs — Browser-Grade UI/UX (Behavior Spec)

- **Status:** Approved (open-design session 2026-06-12)
- **Date:** 2026-06-12
- **Surface:** `crates/spur-notebook/jute-notebook/src` (tab strip, menus, previews). Frontend only;
  the daemon focus/close/keyed-registry layer from the 2026-06-09 spec is already shipped.
- **Builds on:** `docs/superpowers/specs/2026-06-09-notebook-multi-tab-design.md` (approved §4 UI/UX)
- **Approved visual:** scratch notebook `~/.spur/scratch/Untitled115.ipynb` (interactive design
  board: live mock, 12-state board, behavior tables, browser-mapping rationale)

## 1. Problem

The shipped tab strip implements the 2026-06-09 anatomy (kernel dot, badge, dirty/✕ swap,
confirm-on-close, ⌘T/⌘W/⌘1-9) but is missing the behavior layer browsers converged on:
overflow tabs are clipped with no recovery path, no drag-to-reorder, no pinning, no hover
preview, no context menu, no reopen-closed-tab, and the ▾ menu only offers New/Open.

## 2. Adopted behaviors (browser practice → Jute)

1. **Dynamic width:** tabs share the strip from 200px max down to a 56px floor. Progressive
   disclosure as tabs narrow: close slot hides below ~96px, language badge below ~66px; the
   kernel dot never yields. Pinned tabs are exempt from shrinking.
2. **Width-lock on close (Chrome):** after closing via ✕ or middle-click, remaining tabs hold
   their widths until the pointer leaves the strip, so repeated closes hit the same spot.
3. **Overflow ladder:** shrink → strip scrolls horizontally (no arrow buttons; trackpad) →
   searchable ▾ tab list. The list is always available, not only on overflow; rows show kernel
   dot, title, dirty dot, and path, with a "current" marker. New notebook / Open notebook stay
   in this panel below the rows.
4. **Pinned tabs:** icon-only 42px (kernel dot + badge), anchored to the left group, no close
   button, protected from ⌘W and Close Others; closing is via the context menu only. Pinned
   order persists in the route (`pinned` query params).
5. **Hover card** after 350ms: filename, full path, kernel state + generation, CPU/RAM
   (fetched via `Notebook.refreshKernelSlotInfo()` on open; "·" while absent), mode, unsaved.
6. **Context menu (v1):** Pin/Unpin, Close (⌘W), Close Others (n), Close to the Right (n),
   Reopen Closed Tab (⌘⇧T), Copy Path. Rendered disabled: Duplicate, Move to New Window.
7. **Reopen closed tab (⌘⇧T):** LIFO stack (cap 10) of `{tab, index}`; reopen restores the
   document at its old index via `open` + `activate:false`; kernel restarts cold.
8. **Middle-click** closes (not pinned). **Double-click empty strip** creates a tab.
9. **Attention state:** a background tab whose kernel transitions out of `running` gets
   `attention` (soft green tint + tick suffix) until activated.
10. **Keyboard:** ⌘T new · ⌘W close (skips pinned) · ⌘⇧T reopen · ⌘1-8 jump · ⌘9 last tab ·
    ⌃Tab / ⌃⇧Tab cycle · ⌘⌥←/→ kept for Safari parity · middle-click close.
11. **Drag to reorder** with drop indicator; pinned tabs are not draggable and unpinned tabs
    clamp to after the pinned group. New order syncs to the route (`path` param order).
12. **`+` travels with the last tab** (Chrome); the right edge holds only the ▾ tab list.

## 3. Deviations from browsers, on purpose

- Confirm-on-close stays for dirty/running tabs (closing is kernel teardown).
- Violet remains exclusive to the agent/annotation layer (◎, dirty dot).
- Close Others / Close to the Right use ONE batch confirm when any target is dirty/running.

## 4. Deferred (need backend wiring; not in this frontend plan)

- Context-menu kernel verbs (Restart Kernel, Shut Down Kernel keep tab), Reveal in Finder.
- Background-agent ◎ decoupled from the active tab (needs an agent-focus event distinct from
  `set_focus` follow-active).
- Tab tear-off to a new window; Duplicate.
- Keep-warm choice on unpinned close (pinning answers the §10 keep-warm question for v1).

## 5. Visual tokens

Locked to the approved 2026-06-09 language: `gray-50` chrome, `gray-200` hairlines, `gray-900`
ink, `gray-500` muted, green/orange kernel dots (pulse while running), violet agent layer,
mono badges. Attention tint: `green-50` background + green tick. Drop indicator: 2px `gray-900`.
