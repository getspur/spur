# spur Community tier

When you install spur with no LicenseSeat configuration, it runs on the **Community** tier — a free tier that includes the full daily-driver workflow:

| Capability | Feature key / quota | Community |
|---|---|---|
| Brain-session orchestration with worker delegation | `core_core_brain_session` | ✓ |
| Single concurrent worker | `core_core_parallel_workers` (cap = 1 via `max_concurrent_workers` quota) | ✓ |
| Worktree isolation per delegation | `worktree_core_isolation` | ✓ |
| Manual review gate (approve / reject / modify) | `core_core_review` | ✓ |
| Session resume from lineage replay | `core_core_session_resume` | ✓ |
| Full TUI: dashboard, session detail, plan inspector, issue browser | `tui_core_view_*` | ✓ |
| Ad-hoc CLI runs (`spur run`, `spur exec`, `spur cost`) | `cli_core_run`, `cli_core_exec`, `cli_core_cost`, … | ✓ |
| Local PM browse and beads-basic integration | `pm_core_browse`, `pm_core_beads_basic` | ✓ |
| Local beads-backed plan persistence and mutation | `pm_pro_beads_advanced` | ✓ |
| MCP delegate / PR-creation tools | `mcp_core_delegate`, `mcp_core_pr` | ✓ |

Pro adds: parallel workers within one orchestrator (`max_concurrent_workers = 10`), per-project cost analytics (`cost_pro_per_project_tracking`, `ctx_pro_duckdb_engine`), Telegram remote review (`bot_pro_telegram_solo`, `bot_pro_inline_review`), and automation policies (`core_pro_review_auto_approve`, `skills_pro_custom`).

The canonical feature list lives in [`crates/spur-license/resources/default_policy.json`](../../crates/spur-license/resources/default_policy.json) under `tier_policies`. It is signed (Ed25519) and verified at compile time and runtime, and it is the single source of truth for tier entitlements — there are no runtime feature grants outside the signed policy.

## Multiple SPUR TUIs per repo

Community allows multiple `spur tui` processes to run against the same repository. TUI startup does not acquire a tier-specific repository pidfile, so another running Community TUI does not block a new one.

The signed Community policy still limits each orchestrator to one concurrent worker (`max_concurrent_workers = 1`). Multiple TUI processes therefore provide process-level concurrency, but they do not share cross-instance state coordination. Pro adds up to 10 concurrent workers within one orchestrator with shared lineage.

## Upgrading to Pro

Run `spur auth login --key <YOUR-KEY>` once you have a license key. The tool persists the activation locally and the next `spur tui` comes up as Pro.

If you don't have a key yet, purchase a Pro license through the SPUR vendor portal and run the activation command above. Community remains free, with no time limit.
