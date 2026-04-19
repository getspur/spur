# LicenseSeat 0.5.3 Emission & Policy Audit

**Pinned crate source:** `/Users/kevintruong/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/licenseseat-0.5.3` (hash-bearing cargo registry directory)
**Relative citations below are to paths under that root.**

## Gate 1 — Does the SDK emit on explicit handler calls?

**Answer:** CONFIRMED

Every one of the four explicit handler methods calls `self.emit(...)` on both the success and error paths. `emit` is a private helper (defined at `src/client.rs:1628`) that directly sends on the internal `broadcast::Sender<Event>` field — the same channel exposed by `subscribe()`. There is no autonomous-timer separation; the emission is unconditional within the method call.

Per-method evidence:

- `activate(...)` — emits at `src/client.rs:164` (`ActivationStart`), `src/client.rs:193` (`ActivationSuccess`), and `src/client.rs:216` (`ActivationError`). Delegates to `activate_with_options`, which does the actual sends.
- `validate(...)` — emits at `src/client.rs:245` (`ValidationStart`), `src/client.rs:275` (`ValidationSuccess`) or `src/client.rs:281` (`ValidationFailed`), and `src/client.rs:297` (`ValidationError`). Delegates to `validate_key`.
- `heartbeat(...)` — emits at `src/client.rs:448` (`HeartbeatSuccess`) and `src/client.rs:458` (`HeartbeatError`). Delegates to `heartbeat_key`.
- `deactivate(...)` — emits at `src/client.rs:360` (`DeactivationStart`), `src/client.rs:370` / `src/client.rs:381` / `src/client.rs:398` (`DeactivationSuccess`), and `src/client.rs:406` (`DeactivationError`). Delegates to `deactivate_key`.

Relevant code excerpts:

- `src/client.rs:1628`:
  ```rust
  fn emit(&self, event: Event) {
      let _ = self.inner.event_tx.send(event);
  }
  ```

- `src/client.rs:61` and `src/client.rs:89`:
  ```rust
  event_tx: broadcast::Sender<Event>,
  // ...
  let (event_tx, _) = broadcast::channel(64);
  ```

- `src/client.rs:895`:
  ```rust
  pub fn subscribe(&self) -> broadcast::Receiver<Event> {
      self.inner.event_tx.subscribe()
  }
  ```

- `src/client.rs:164` (activate emit):
  ```rust
  self.emit(Event::new(EventKind::ActivationStart));
  ```

- `src/client.rs:193` (activate success emit):
  ```rust
  self.emit(Event::with_license(
      EventKind::ActivationSuccess,
      license.clone(),
  ));
  ```

- `src/client.rs:245` (validate emit):
  ```rust
  self.emit(Event::new(EventKind::ValidationStart));
  ```

- `src/client.rs:275` (validate success emit):
  ```rust
  self.emit(Event::with_validation(
      EventKind::ValidationSuccess,
      result.clone(),
  ));
  ```

- `src/client.rs:448` (heartbeat success emit):
  ```rust
  self.emit(Event::new(EventKind::HeartbeatSuccess));
  ```

- `src/client.rs:360` (deactivate emit):
  ```rust
  self.emit(Event::new(EventKind::DeactivationStart));
  ```

**Consequence:** When SPUR calls any of these four methods explicitly, the SDK fires events through `event_tx` before returning. If `spawn_sdk_event_bridge` is already subscribed via `sdk.subscribe()`, it receives those same events. SPUR's `replace_state` also sends on the same SPUR-level broadcast channel after the explicit call returns. The C9 duplicate-emission concern is real: every explicit `activate`/`validate`/`heartbeat`/`deactivate` will fire both the SDK-bridge path and the `replace_state` path.

**EventKind deduplication reference for Task 7's implementer:**

Kinds that explicit handlers already broadcast via `replace_state` (bridge must drop these — they are emitted inside the four public API methods, all of which SPUR calls explicitly):

| EventKind | Upstream emit site | SPUR mapping (`map_event_kind`) |
|---|---|---|
| `ActivationSuccess` | `src/client.rs:193` | `LicenseEventKind::Activated` |
| `ActivationError` | `src/client.rs:216` | `LicenseEventKind::ActivationFailed` |
| `ValidationSuccess` | `src/client.rs:275` | `LicenseEventKind::Validated` |
| `ValidationFailed` | `src/client.rs:281` | `LicenseEventKind::ValidationFailed` |
| `ValidationError` | `src/client.rs:297` | `LicenseEventKind::ValidationFailed` |
| `ValidationAuthFailed` | `src/client.rs:292` (inside `validate_key`) | `LicenseEventKind::ValidationFailed` |
| `HeartbeatSuccess` | `src/client.rs:448` | `LicenseEventKind::HeartbeatOk` |
| `HeartbeatError` | `src/client.rs:458` | `LicenseEventKind::HeartbeatFailed` |
| `DeactivationSuccess` | `src/client.rs:370`, `381`, `399` | `LicenseEventKind::Deactivated` |
| `DeactivationError` | `src/client.rs:406` | `LicenseEventKind::DeactivationFailed` |

Note: `ActivationStart`, `ValidationStart`, and `DeactivationStart` are also emitted inside the explicit handler calls but have no SPUR `replace_state` equivalent — the bridge may forward or drop them as informational only (they carry no state transition).

Kinds the bridge **must** forward (autonomous / server-push — emitted outside the four explicit handler methods, in background threads or startup):

| EventKind | Upstream emit site | Origin | SPUR mapping |
|---|---|---|---|
| `LicenseRevoked` | `src/client.rs:301` (inside `validate_key`, called by the auto-validation loop) | Server push detected during background auto-validation | `LicenseEventKind::ValidationFailed` |
| `ValidationAutoFailed` | `src/client.rs:981`, `992` (inside `start_auto_validation` background thread) | Autonomous background check | `LicenseEventKind::ValidationFailed` |
| `OfflineValidationFailed` | `src/client.rs:1751` (inside `validate_offline`, triggered by auto-validation fallback) | Autonomous background check | `LicenseEventKind::ValidationFailed` |
| `ValidationOfflineFailed` | `src/client.rs:1755` (co-emitted with `OfflineValidationFailed`) | Autonomous background check | `LicenseEventKind::ValidationFailed` |
| `MachineFileVerificationFailed` | `src/client.rs:1533` (inside `verify_machine_file`, called from offline validation path) | Autonomous background check | `LicenseEventKind::ValidationFailed` |
| `OfflineTokenVerificationFailed` | `src/client.rs:1473` (inside `verify_offline_token`, called from offline validation path) | Autonomous background check | `LicenseEventKind::ValidationFailed` |
| `LicenseLoaded` | `src/client.rs:116` (SDK constructor, on cold-start cache hit) | SDK startup | `_` (wildcard → `LicenseEventKind::Validated`) |
| `NetworkOnline` / `NetworkOffline` | `src/client.rs:1635-1638` (inside `set_online`, called from background support tasks and on network-error detection) | Autonomous network monitor | `_` (wildcard → `LicenseEventKind::Validated`) |

The `_` wildcard arm in `map_event_kind` (`crates/spur-license/src/licenseseat.rs:347`) currently maps all unrecognized kinds to `LicenseEventKind::Validated`, which may produce spurious events for `LicenseLoaded` and `NetworkOnline/Offline`. Task 7's implementer should decide whether to filter these before forwarding.

Implication for Phase 2 Task 7 (C9 dedup): **EXECUTE**

---

## Gate 2 — Which binding modes require heartbeat?

**Answer:** The upstream crate does not define a `BindingMode` enum. License mode is a raw `String` field with three documented values. The `start_background_tasks` function starts heartbeats unconditionally for all modes; there is no per-mode gating in the SDK.

Upstream mode values (from `src/models.rs:51`):
- `"hardware_locked"` — device-bound, single-seat
- `"floating"` — concurrent seat checked out, released when heartbeats stop
- `"named_user"` — user-bound seat

Upstream-to-SPUR mode mapping (inferred from docs and crate comment):
- upstream `"hardware_locked"` → SPUR `NodeLocked`
- upstream `"floating"` → SPUR `FloatingCi`
- upstream `"named_user"` → SPUR `Organization` (closest match; named-user = org identity)

Evidence — the mode field is a plain string, not an enum:
- `src/models.rs:51`:
  ```rust
  /// License mode ("hardware_locked", "floating", "named_user").
  pub mode: String,
  ```

Evidence — `start_background_tasks` calls `start_heartbeat` unconditionally for every mode:
- `src/client.rs:907`:
  ```rust
  pub fn start_background_tasks(&self) {
      let Some(license) = self.inner.cache.get_license() else {
          debug!("No active license, skipping background task startup");
          return;
      };

      self.start_auto_validation(&license.license_key);
      self.start_heartbeat(&license.license_key);
      self.start_support_tasks();
  }
  ```

Evidence — the README documents heartbeats as the mechanism for releasing `floating` seats but does not exempt `hardware_locked` or `named_user` from the heartbeat loop. The config default also shows heartbeat always enabled unless `heartbeat_interval` is set to zero:
- `src/config.rs:70`:
  ```rust
  /// Interval for standalone heartbeat pings.
  /// Set to zero to disable auto-heartbeat.
  /// Default: 5 minutes
  pub heartbeat_interval: Duration,
  ```
- `src/config.rs:146`:
  ```rust
  heartbeat_interval: Duration::from_secs(300),      // 5 minutes
  ```

Evidence — README `Heartbeat & Seat Tracking` section (`README.md:300`):
```
If heartbeats stop (app crash, network loss, user closes app), the seat is released after the grace period configured in your LicenseSeat dashboard.
```

The README frames heartbeat as a universal seat-tracking mechanism, not one conditional on the floating mode.

**Analysis:** The upstream crate 0.5.3 treats heartbeat as a global opt-out-via-zero-interval feature rather than a per-mode gate. There is no source-level evidence that any of the three modes exempts a license from requiring heartbeats per the upstream lease model. The SDK starts heartbeats for every activated license regardless of `mode`. Therefore, SPUR's D1 concern — gating heartbeat only for `FloatingCi` — is NOT supported by the upstream contract as implemented. Setting `heartbeat_interval = 0` disables heartbeats globally; mode-conditional gating requires SPUR to implement it on top of the SDK, as the SDK does not do it natively.

Implication for Phase 2 Task 8 (D1 gating): **SKIP**

The upstream model does not differentiate heartbeat requirements by mode. All activated subjects run heartbeats at the configured interval. Task 8's per-mode gate would be a SPUR-layer policy, not an enforcement of any upstream invariant.

**Interaction with SPUR's existing `should_heartbeat` gate:** SPUR already has a coarser SPUR-layer gate at `crates/spur-core/src/license_runtime.rs:79-81`:

```rust
fn should_heartbeat(state: &LicenseState) -> bool {
    state.is_active() && !matches!(state.binding_mode, BindingMode::Unknown)
}
```

This guard suppresses heartbeats when `BindingMode` is `Unknown` (i.e., before SPUR has resolved the mode from the SDK response). It is **intentionally retained as-is**: its purpose is to prevent heartbeat calls before a license is fully bound, not to restrict heartbeat to any specific mode. Task 8 is SKIP because no upstream invariant motivates tightening this gate further — for example, restricting it to `FloatingCi`-only — since the upstream SDK runs heartbeats for all modes equally. If SPUR policy later decides that `NodeLocked` or `Organization` seats should not heartbeat, that change would need to be implemented explicitly in this guard; the audit found no upstream contract that mandates it.

---

## Gate 3 — Is cached plan_key + entitlements available pre-network?

**Answer:** YES, but only via an optional nested field — not a direct top-level field on `License`.

`current_license()` returns `Option<License>` (read from disk cache with no network call):
- `src/client.rs:601`:
  ```rust
  /// Get the current cached license.
  pub fn current_license(&self) -> Option<License> {
      self.inner.cache.get_license()
  }
  ```

The `License` struct does NOT directly contain `plan_key` or `active_entitlements`:
- `src/models.rs:253`:
  ```rust
  pub struct License {
      pub license_key: String,
      pub device_id: String,
      pub activation_id: String,
      pub activated_at: DateTime<Utc>,
      pub last_validated: DateTime<Utc>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub trusted_license: Option<LicenseResponse>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub validation: Option<ValidationResult>,
  }
  ```

`plan_key` and `active_entitlements` live on `LicenseResponse`, accessible as:
- `license.trusted_license.as_ref()?.plan_key` — populated after every successful activate, validate, or heartbeat response, and persisted to disk as part of the `License` JSON blob (the field is not `skip_serializing_if` absent — it is serialized when `Some`).
- `license.validation.as_ref()?.license.plan_key` — populated after validate.

The `LicenseResponse` struct:
- `src/models.rs:38`:
  ```rust
  pub struct LicenseResponse {
      pub object: String,
      pub key: String,
      pub status: String,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub starts_at: Option<DateTime<Utc>>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub expires_at: Option<DateTime<Utc>>,
      /// License mode ("hardware_locked", "floating", "named_user").
      pub mode: String,
      /// License plan key.
      pub plan_key: String,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub seat_limit: Option<u32>,
      pub active_seats: u32,
      /// List of active entitlements.
      pub active_entitlements: Vec<Entitlement>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub metadata: Option<HashMap<String, serde_json::Value>>,
      pub product: Product,
  }
  ```

**Persistence chain:** On activation (`src/client.rs:192`), the SDK writes `License { trusted_license: Some(activation.license.clone()), .. }` to disk. On heartbeat (`src/client.rs:444`), it calls `cache.set_trusted_license(&response.license)` which overwrites `license.trusted_license` on the stored record. On validate (`src/client.rs:254`), it calls `cache.set_license_snapshot` and `cache.update_validation` (which sets `license.trusted_license` when valid). Therefore:

- After the **first** successful activate, `current_license()?.trusted_license` contains `plan_key` + `active_entitlements` and is persisted on disk.
- On **cold start** after a prior successful session, `current_license()?.trusted_license` is populated from disk with no network call.
- On **first-ever cold start** (no prior cache), `current_license()` returns `None`.

**Conclusion:** `plan_key` + `active_entitlements` ARE available pre-network on any cold start after the first activation. SPUR's `active_cached()` (which currently sets `Plan::Unknown`) can be fixed to walk `license.trusted_license?.plan_key` from the disk-cached `License` without a round-trip.

Implication for Phase 2 Task 6 (G2 hydration): **EXECUTE as-written**

---

## Summary decision vector

- Task 6: EXECUTE
- Task 7: EXECUTE
- Task 8: SKIP

---

## Auditor notes

1. **No `BindingMode` enum in upstream.** SPUR's `BindingMode` variants (`NodeLocked`, `FloatingCi`, `Organization`) are SPUR-invented abstractions. The upstream crate uses a raw `String` for `mode` with three undocumented-as-enum values: `"hardware_locked"`, `"floating"`, `"named_user"`. Any mode-to-enum mapping in SPUR is a local convention, not an SDK type.

2. **Heartbeat is unconditional upstream.** The SDK starts heartbeat for all modes; the only way to suppress it is `heartbeat_interval = Duration::ZERO`. Task 8's proposed gate "only heartbeat for FloatingCi" would be purely SPUR-layer policy. If implemented, SPUR must suppress `start_heartbeat` for non-floating seats rather than relying on any upstream invariant. Skipping Task 8 is correct because the upstream does not discriminate — but if SPUR policy later decides hardware-locked seats should not heartbeat, SPUR must implement that gate explicitly.

3. **Emit helper is synchronous.** `self.emit(event)` calls `broadcast::Sender::send` synchronously within the method body, before the method returns. This means the SDK bridge subscriber receives all events before SPUR's own `replace_state` runs — the C9 dedup window is deterministic and does not involve race conditions.

4. **`trusted_license` vs `validation.license` for G3.** Two paths to `plan_key` exist on the cached `License`. SPUR should prefer `trusted_license` (set after activate/heartbeat) over `validation.license` (set only after validate), because heartbeat responses also refresh it and heartbeat is more frequent than validation.

5. **Crate version note.** The `0.5.3` source does not expose a `LicenseMode` typed enum from `pub use` in `src/lib.rs`. If a future version introduces one, the no-enum finding here would need re-auditing.
