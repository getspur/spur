# Spur Commercial Licensing Architecture & Strategy

## Objective
Establish a rigorous, performant, and secure commercial licensing architecture for the `spur` Rust TUI/CLI application. The system must support high-velocity zero-latency startup, offline viability (for air-gapped or CI/CD usage), machine fingerprinting, and flexible marketing capabilities (coupons, giveaways, and trials).

## First Principles Constraints
1. **Zero-Latency Startup:** The TUI must render instantly. Synchronous network validation blocking the `main()` thread is unacceptable.
2. **Offline Viability:** The license must be capable of local, offline verification to support remote servers and CI pipelines.
3. **Cryptographic Trust:** The security model must rely on asymmetric cryptography (Ed25519). The licensing vendor signs a payload; the `spur` binary embeds the public key to verify it locally.
4. **Node Locking vs. Floating:** Licenses must be tied to a machine (fingerprinting) to prevent abuse, but also support "floating" cryptographic validation for CI/CD environments where hardware profiles rotate dynamically.

## Vendor Evaluation

### Candidate 1: LicenseSeat
*   **Overview:** A Licensing-as-a-Service platform explicitly built for desktop/CLI apps.
*   **Rust Support:** Provides a native, official `licenseseat` crate (v0.5+).
*   **Pros:** Seamless offline Ed25519 validation, built-in device fingerprinting, and entitlement gating. The native SDK abstracting the cryptographic logic significantly accelerates time-to-market.
*   **Cons:** A specialized, newer SaaS vendor compared to enterprise giants.

### Candidate 2: Keygen.sh
*   **Overview:** The industry gold standard for software licensing (Tailwind UI, Label Sync).
*   **Rust Support:** No official, high-level Rust SDK. Requires custom API wrappers and cryptographic implementations using `reqwest` and `ed25519-dalek`.
*   **Pros:** Incredible flexibility, massive scale, deep enterprise features (air-gapped QR activation, floating concurrency).
*   **Cons:** High implementation overhead for a solo/small team building a Rust CLI.

### Candidate 3: Build-It-Yourself (Ed25519 + Stripe)
*   **Overview:** Generating Ed25519 keypairs and verifying them locally while building a custom SaaS backend.
*   **Pros:** Total control, zero SaaS fees (excluding Stripe).
*   **Cons:** Requires building a complete customer portal, machine tracking database, and revocation infrastructure. A massive distraction from the core `spur` product.

### Selection
**LicenseSeat** (or a custom Keygen implementation adhering to the same cryptographic principles) is the optimal path. The native Rust SDK (`licenseseat`) handles the complex offline verification and fingerprinting required to maintain sub-millisecond CLI startup performance without building bespoke backend infrastructure.

## Marketing & Promotion Strategy (Coupons, Giveaways, Trials)

A critical architectural realization is the hard boundary between **Billing** and **Licensing**. 
*   **Billing/MoR (Stripe, Lemon Squeezy):** Handles money, taxes, **coupons**, and **discount campaigns** (e.g., "50% OFF").
*   **Licensing (LicenseSeat, Keygen):** Handles cryptographic entitlements, machine activations, and **giveaways**.

### 1. Giveaways (100% Free Access)
To distribute free lifetime/annual licenses (e.g., to beta testers or Twitter followers):
1.  Create a specific "Policy" in LicenseSeat (e.g., `Spur Pro - Early Adopter`).
2.  Use the LicenseSeat dashboard/API to manually generate a batch of 50 unique keys tied to that policy.
3.  Distribute keys via email/DM.
4.  The user runs `spur auth login <KEY>`. LicenseSeat activates the key because it is payment-agnostic.

### 2. Discount Campaigns (e.g., "BLACKFRIDAY50")
To run a 50% discount campaign:
1.  Create the coupon code inside the payment gateway (Lemon Squeezy/Stripe).
2.  The user applies the coupon at checkout, paying a reduced price.
3.  Upon successful payment, the gateway fires a webhook (or direct integration) to LicenseSeat.
4.  LicenseSeat blindly generates and emails 1 license key, completely unaware of the price paid.

### 3. Time-Limited Trials (Try Before You Buy)
To offer a frictionless 14-day trial:
1.  Create a Policy in LicenseSeat configured to expire exactly 336 hours after activation.
2.  Generate a universal "Global Key" (e.g., `SPUR-FREE-TRIAL`) with strict node-locking (1 activation per machine fingerprint).
3.  Add a command: `spur auth trial`. Under the hood, this calls `licenseseat::activate("SPUR-FREE-TRIAL", machine_fingerprint)`.
4.  If the user attempts to run `spur auth trial` again on the same machine after 14 days, the SaaS rejects the fingerprint.

## Spur Integration Architecture

To integrate commercial licensing into `spur` without degrading the TUI experience:

### 1. Crate Isolation
Create `crates/spur-license` to encapsulate the `licenseseat` SDK and the embedded Ed25519 public key (`include_bytes!("public.pem")`).

### 2. The Zero-Latency Startup Loop
1.  **Sync Read:** `spur-license` synchronously reads `~/.spur/license.key` (or checks `SPUR_LICENSE_KEY`).
2.  **Sync Crypto Verify:** Performs offline Ed25519 signature verification (~50 microseconds).
3.  **State Hydration:** Deserializes entitlements (e.g., `{"tier": "pro"}`) and passes `LicenseState` to `spur-tui`. The TUI boots instantly.
4.  **Async Revocation Polling:** A background Tokio task pings the SaaS API (`licenseseat::ping()`). If revoked, it deletes the local key and updates the TUI state via an `mpsc` channel to downgrade the user gracefully.

### 3. The CLI UX
*   **Activation:** `spur auth login --key XXXX-XXXX-XXXX`
*   **Trial:** `spur auth trial`
*   **CI/CD (Floating):** Set `SPUR_LICENSE_KEY=XXXX...` in the environment. The offline verifier bypasses machine fingerprinting and validates the signature directly.
