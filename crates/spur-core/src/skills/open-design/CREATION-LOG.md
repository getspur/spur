# open-design — creation log

- **2026-05-31** — Created for the "Open Design on Jute" host-shell M1 vertical slice.
  Re-homes Open Design's prompt stack (discovery / directions / critique) as a
  notebook-driven SPUR skill. Source spec:
  `docs/superpowers/specs/2026-05-31-open-design-jute-host-shell-design.ipynb`.
  Replaces OD's Node daemon agent loop with `notebook_*` MCP tool driving.
  Distribution: brain-role, so `skills init` materializes it only under
  `.spur/skills/open-design/` (worker adapter dirs are intentionally skipped).
