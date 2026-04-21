# Try Pro features

We provide a public, rate-limited demo key so you can evaluate Pro without signing up:

```
spur auth login --key DEMO-SPUR-2026-Q2
```

**Demo key details:**
- Wall-clock expiry: 2026-07-01 (rotated quarterly).
- Activation rate-limited tenant-side.
- All Pro entitlements unlocked for the duration.

When the key expires, `spur` automatically falls back to the Community tier. To stay on Pro, purchase a license from your team's vendor portal and run `spur auth login --key <REAL-KEY>`.
