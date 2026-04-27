# Brainstorm: LicenseSeat Admin Architecture via MCTS + First Principles

**Goal:** Design a LicenseSeat management layer for SPUR where the **admin build never distributes or leaks secrets**.

**Method:** Monte Carlo Tree Search (MCTS) over an architectural decision tree, guided by first-principles security decomposition.

---

## 1. First Principles (The "Physics" of the Problem)

Before branching, we decompose to invariant truths:

| Principle | Implication for SPUR |
|-----------|---------------------|
| **P1: Build artifacts are untrusted execution environments** | CI runners, caches, logs, and layer histories can be inspected, dumped, or accidentally published. |
| **P2: A secret that enters a build graph can be extracted from the build graph** | If `SPUR_POLICY_SIGNING_KEY` is an env var in CI, it is recoverable via `strings`, `/proc/self/environ`, or cache introspection. |
| **P3: Verification keys are safe to embed; signing keys are not** | `spur-policy-2026-04.pub` (32 bytes) can be `include_bytes!`'d. The corresponding private key must never be in the distributable build closure. |
| **P4: LicenseSeat `pk_*` ≠ secret; `sk_*` = secret** | The runtime SDK uses a **publishable** key by design. But admin operations (creating licenses, reading analytics) require the **secret** key. |
| **P5: Code that can sign policy can forge any entitlement** | Policy signing is a root-equivalent capability in SPUR's feature-gate model. Its key must have the smallest possible blast radius. |
| **P6: Separation of privileges is cheaper than revocation** | Rotating a leaked Ed25519 key across all installed binaries is operationally expensive. Preventing exposure is cheaper. |

---

## 2. MCTS Decision Tree

We treat each architectural choice as a node. We evaluate via **rollout** (simulating the future consequence) and backpropagate a security score.

### Root Node
**State:** SPUR needs to (a) validate LicenseSeat licenses at runtime, (b) manage LicenseSeat licenses via admin, (c) sign tier-policy documents, without leaking admin credentials through the build pipeline.

---

### Branch A: Monolithic Build (Current Trajectory)
**Action:** Keep everything in `spur-license` and `spur-cli`. CI runs `sign-policy.sh` with `SPUR_POLICY_SIGNING_KEY` at build time.

**Rollout Simulation:**
- CI env var `SPUR_POLICY_SIGNING_KEY` is injected.
- `sign-policy.sh` calls `openssl pkeyutl -sign`.
- Key material exists in CI process memory, shell history, and potentially action logs.
- Distributable binary contains only the public key + signed policy (safe).
- **But the build environment itself is tainted.**

**Outcome Leaf:**
- **Leak probability:** High over 12 months (CI misconfig, log retention, supply-chain attack on runner).
- **Impact:** Attacker forges `default_policy.json`, grants themselves `enterprise` tier, distributes cracked SPUR binary.
- **Score:** **0.15 / 1.0** (Exploitation: low; Exploration: finds that the build is the weak link).

**UCT Verdict:** Dead branch. Do not descend.

---

### Branch B: Pre-signed Artifact (Sign-Then-Build)
**Action:** Admin signs `default_policy.json` locally. Only the signed artifact + public key enter the repo/CI. CI verifies but never signs.

**Rollout Simulation:**
- Admin runs `sign-policy.sh` on their local machine (or HSM).
- Commits `resources/default_policy.json` (already signed) to git.
- CI `build.rs` verifies signature against embedded `spur-policy-2026-04.pub`.
- CI never sees the private key.

**Outcome Leaf:**
- **Leak probability:** Key never enters CI. Leak requires admin machine compromise.
- **Impact:** Forgery requires stealing the admin's signing key directly.
- **Operational cost:** Manual step per policy change.
- **Score:** **0.75 / 1.0**

**UCT Verdict:** Strong. Solves the build-leak problem for policy signing. But we still need to solve LicenseSeat admin operations.

---

### Branch C: Dual Binary Split (Runtime vs Admin)
**Action:** Split into two artifacts:
1. **`spur`** (runtime): Embeds `pk_*`, public policy key, signed policy. Zero admin capability.
2. **`spur-license-admin`** (admin): Contains `sk_*` or LicenseSeat dashboard session auth. Can sign policies, generate keys, view analytics. Never shipped to end users.

**Rollout Simulation:**
- `spur` build pipeline is clean: no secrets, reproducible, safe to distribute.
- `spur-license-admin` build pipeline is separate. It may contain `sk_*` or OAuth tokens, but it is built locally by the admin or in a locked-down internal pipeline.
- Admin binary is distributed via internal channel (1Password/MDM), not GitHub releases.

**Outcome Leaf:**
- **Leak probability:** Near-zero for the runtime binary. Admin binary leak is possible but contained to admin staff.
- **Impact:** Even if `spur-license-admin` leaks, `spur` runtime binaries are unaffected.
- **Operational cost:** Two build targets, two release streams.
- **Score:** **0.90 / 1.0**

**UCT Verdict:** Best path. Combines Branch B's pre-signed safety with privilege separation.

---

### Branch D: HSM/Vault-Signed Build
**Action:** CI requests policy signatures from a remote HSM or Vault (e.g., AWS KMS, HashiCorp Vault, YubiHSM). CI never sees the private key.

**Rollout Simulation:**
- CI sends policy payload to HSM over TLS/mTLS.
- HSM returns signature. CI assembles `SignedPolicy`.
- Private key is non-exportable.

**Outcome Leaf:**
- **Leak probability:** Key extraction is cryptographically prevented.
- **Impact:** If HSM is compromised, impact is catastrophic but probability is very low.
- **Operational cost:** HSM procurement, network dependency for builds, IAM complexity.
- **Score:** **0.85 / 1.0**

**UCT Verdict:** Excellent security, but high operational overhead. May be overkill for SPUR's current stage. Can be adopted later as an upgrade to Branch C.

---

### Branch E: Runtime-Only Overlay (No Embedded Policy)
**Action:** Remove embedded policy. At startup, `spur` fetches signed policy from a SPUR-controlled HTTPS endpoint.

**Rollout Simulation:**
- Binary has no static entitlements. Requires network to boot.
- Offline/air-gapped users cannot start SPUR.
- Central server becomes a kill-switch and a DDoS target.

**Outcome Leaf:**
- **Leak probability:** No signing key in build, but central server is a new attack surface.
- **Impact:** Complete denial of service if endpoint is down or user is offline.
- **Score:** **0.40 / 1.0**

**UCT Verdict:** Rejected. Violates offline-first requirement implied by Community tier and local tooling.

---

## 3. Backpropagation: The Winning Path

After simulating all branches, the **optimal subtree** is:

```
Root
├── Branch B: Pre-signed Artifact (policy layer)
│   └── Branch C: Dual Binary Split (admin layer)
│       └── Optional Branch D: HSM upgrade (future)
```

**Why this wins:**
- **Exploitation:** Maximizes the "never leak" constraint (score 0.90+).
- **Exploration:** Leaves room to upgrade to HSM later without changing the runtime binary architecture.
- **Minimax:** Even in the worst case (admin machine compromised), the blast radius is limited to policy forgery — the runtime binary and its users are not at risk.

---

## 4. Concrete Architecture for SPUR

### 4.1 Artifact Separation

```
crates/
├── spur-license/          # Shared library (policy, validation, gates)
├── spur-cli/              # Runtime CLI (what users run)
└── spur-license-admin/    # Admin CLI (what SPUR team runs locally)
```

**`spur-cli` (Distributable)**
- Embeds `pk_live_*` (publishable, safe).
- Embeds `spur-policy-*.pub`.
- Embeds pre-signed `default_policy.json`.
- Calls `LicenseSeat::validate()`, `has_entitlement()`.
- **Cannot** create licenses, sign policies, or call admin API endpoints.

**`spur-license-admin` (Internal Only)**
- Links `spur-license` + admin-only modules.
- Reads `SPUR_LICENSESEAT_SECRET_KEY` (`sk_*`) from 1Password / env (local only).
- Reads `SPUR_POLICY_SIGNING_KEY` from YubiKey / local file (never from CI).
- Commands:
  - `sign-policy <json> --key-id spur-policy-2026-04` → outputs `SignedPolicy`
  - `license create --plan pro --email user@example.com`
  - `license revoke --key XXXX-XXXX`
  - `seat list --key XXXX-XXXX`
  - `analytics dump`

### 4.2 Policy Update Flow (Sign-Then-Build)

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Admin Machine  │────▶│  Git Repository  │────▶│  CI Pipeline    │
│  (air-gapped    │     │  (signed policy  │     │  (verify only)  │
│   or local)     │     │   + pubkey)      │     │                 │
└─────────────────┘     └──────────────────┘     └─────────────────┘
         │                                                  │
         │ SPUR_POLICY_SIGNING_KEY                          │ include_bytes!
         ▼                                                  ▼
┌─────────────────┐                               ┌─────────────────┐
│  sign-policy.sh │                               │  spur binary    │
│  (local exec)   │                               │  (distributed)  │
└─────────────────┘                               └─────────────────┘
```

**Invariant:** The arrow from "Admin Machine" to "CI Pipeline" does not exist for the private key.

### 4.3 Build Hardening Rules

| Rule | Enforcement |
|------|-------------|
| `spur-cli` build fails if `SPUR_POLICY_SIGNING_KEY` is present in env | `build.rs` assertion |
| `spur-license-admin` build fails if `SPUR_LICENSESEAT_SECRET_KEY` is missing | compile_error or runtime check |
| CI pipeline for `spur-cli` has no secret key env vars | GitHub Actions `env:` audit |
| `spur-license-admin` is excluded from release workflow | `.github/workflows/release.yml` filter |

### 4.4 Key Rotation Forward-Compatibility

The existing `trusted_keys()` map in `trust.rs` already supports multi-key:

```rust
pub fn trusted_keys() -> &'static BTreeMap<&'static str, VerifyingKey> {
    // Add new key BEFORE rotating issuance;
    // remove old key in later release.
}
```

This allows:
1. Ship a `spur` binary that trusts both `spur-policy-2026-04` and `spur-policy-2026-10`.
2. Admin starts signing with the new key.
3. Later release drops the old key.

This is a **keychain** model, not a single-key model. It enables graceful rotation without breaking installed binaries.

---

## 5. MCTS: What We Explored vs What We Exploited

| Explored (Novel Paths) | Exploited (Best Returns) |
|------------------------|--------------------------|
| HSM/Vault signing (Branch D) | Pre-signed artifact (Branch B) — eliminates CI secret exposure |
| Runtime-only policy (Branch E) | Dual binary split (Branch C) — separates admin privilege from runtime |
| Embedded admin mode with compile-time gating | Public key embedding + `build.rs` verification — existing strength |

The "exploit" phase of MCTS tells us: **the highest-return move is removing the signing key from the build closure entirely.** Everything else (HSM, admin binaries, rotation) is either an extension of that move or a defense-in-depth layer.

---

## 6. Immediate Action Items

If this brainstorm is adopted:

1. **Create `crates/spur-license-admin/`** as a new binary crate.
2. **Move `sign-policy.sh` logic** into a Rust command in `spur-license-admin` (safer than shell + openssl).
3. **Add `build.rs` guard** to `spur-cli` that panics if `SPUR_POLICY_SIGNING_KEY` is detected in the build environment.
4. **Document the sign-then-build flow** in `AGENTS.md` or `docs/contributing/`.
5. **Audit `.github/workflows/`** to ensure `spur-license-admin` is never built in the release pipeline.
6. **(Future)** Migrate local signing to YubiKey/HSM via `pcsc` or `yubikey-manager` integration in `spur-license-admin`.

---

## 7. Summary

> **First principle:** A build that distributes to end users must not contain, touch, or transit any capability that could forge privileges.
>
> **MCTS best path:** Pre-sign policy artifacts on an admin machine, embed only public verification material in CI, and split admin tooling into a separate non-distributed binary.
>
> **Result:** The `spur` runtime binary is clean, reproducible, and safe. Admin credentials live only where admin actions happen.
