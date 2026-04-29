# spur Community tier

When you install spur with no LicenseSeat configuration, it runs on the **Community** tier — a free tier that includes:

| Feature | Available on Community |
|---|---|
| `chat` | ✓ |
| `code_edit` | ✓ |
| `watch_loop` | ✓ |
| `advanced_agents` | — Pro |
| `cloud_sync` | — Pro |
| `team_sharing` | — Team |

The canonical list lives in [`crates/spur-license/resources/default_policy.json`](../../crates/spur-license/resources/default_policy.json) under `tier_policies`. It is signed (Ed25519) and verified at compile time and runtime.

## Single SPUR per repo

Community enforces one SPUR TUI orchestrator per repository at a time. Launching a second `spur tui` in the same repo will exit cleanly with a message identifying the running process. Read-only commands (`spur auth status`, `spur sessions list`) are not affected.

This keeps Community focused on a single coordinated workflow per project. Pro removes this limit and adds parallel workers within a single orchestrator (up to 10 concurrent) with shared lineage across all of them — the recommended way to run multiple agents simultaneously.

## Upgrading to Pro

Run `spur auth login --key <YOUR-KEY>` once you have a license key. The tool persists the activation locally and the next `spur tui` comes up as Pro.

If you don't have a key yet, purchase a Pro license through the SPUR vendor portal and run the activation command above. Community remains free, with no time limit.
