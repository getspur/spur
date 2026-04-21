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

## Upgrading to Pro

Run `spur auth login --key <YOUR-KEY>` once you have a license key. The tool persists the activation locally and the next `spur watch` comes up as Pro.

If you don't have a key yet, see [Try Pro](try-pro.md).
