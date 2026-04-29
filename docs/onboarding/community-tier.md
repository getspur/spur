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

## Running multiple workflows in parallel

Community runs one worker per spur instance. To work on several things at once for free, open a second terminal and run `spur tui` again — each instance is fully independent. Sessions, lineage, and worktrees are isolated per instance, so multiple instances can run side-by-side on the same machine without colliding.

Pro removes this friction by enabling parallel workers within a single instance (up to 10 concurrent), with shared lineage across all of them.

## Upgrading to Pro

Run `spur auth login --key <YOUR-KEY>` once you have a license key. The tool persists the activation locally and the next `spur tui` comes up as Pro.

If you don't have a key yet, purchase a Pro license through the SPUR vendor portal and run the activation command above. Community remains free, with no time limit.
