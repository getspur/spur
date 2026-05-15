Document version: v0.1 — 2026-05-15. NDA-only.

# SPUR Security Overview

## 1) Architecture & Trust Boundary

SPUR runs as a local orchestration process on the user's machine and uses ACP transports (including subprocess + stdio IPC) to communicate with user-installed local agent runtimes. In the current architecture, SPUR does not directly call first-party LLM provider HTTPS APIs for prompt execution. Instead, prompt traffic is handed to local ACP agents, and those agent processes may then call provider APIs according to the user's own agent configuration and contracts. SPUR's own validated outbound HTTPS path is anonymous telemetry to PostHog. Separately, SPUR's MCP endpoints in current startup paths bind to loopback (`127.0.0.1` on ephemeral ports), not publicly routable interfaces.

```mermaid
flowchart LR
    subgraph UM[User Machine Trust Boundary]
      U[Developer / Operator]
      S[SPUR Process\n(local orchestrator)]
      A1[Local ACP Agent Process\n(Claude Code / Gemini CLI / Codex)]
      M[MCP Endpoint\n127.0.0.1:ephemeral]

      U --> S
      S -->|ACP IPC\n(subprocess + stdio/session RPC)| A1
      S -->|Local loopback HTTP| M
    end

    PH[(PostHog)]
    LLM[(LLM Provider API)]

    S -->|HTTPS telemetry| PH
    A1 -.->|HTTPS prompt/completion traffic\noutside SPUR process boundary| LLM
```

## 2) Data Handling

SPUR's orchestrator role is local routing and control flow, not cloud relay. Based on the verified implementation paths, SPUR prompt execution goes through local ACP sessions to local agent processes. SPUR does not provide a SPUR-hosted prompt relay in the validated architecture.

Within that scope, source code, prompts, and file content remain in the user's local environment unless and until the selected ACP agent transmits data under that agent's own behavior. This distinction matters operationally:

- SPUR process scope: local orchestration, local IPC/session control, optional anonymous telemetry.
- ACP agent scope: model invocation behavior, provider API usage, model-side retention and policy handling (as configured by the user and governed by the user's vendor contract).

Because those upstream provider terms are not SPUR policies, they should be treated as informational pointers only when procurement teams ask how agent-side traffic is governed:

- Anthropic Commercial Terms: <https://www.anthropic.com/legal/commercial-terms>
- OpenAI Enterprise Privacy: <https://openai.com/enterprise-privacy/>

SPUR does not assert those external terms on behalf of any provider; they are references for customers evaluating the agent/provider segment of their stack.

## 3) Telemetry

SPUR's telemetry implementation is in [`crates/spur-telemetry/src`](../../crates/spur-telemetry/src). The default endpoint constant in that crate points to PostHog (`https://us.i.posthog.com`).

Opt-out is explicitly supported via environment variable:

```sh
SPUR_TELEMETRY=0 spur
```

In current runtime logic, an empty `SPUR_TELEMETRY` value or `0` disables telemetry emission for that process. SPUR also supports persisted CLI-based telemetry tier controls (documented in `docs/PRIVACY.md`), but the environment switch above is the direct runtime kill switch.

Telemetry schemas are typed and constrained to operational metadata. Canonical event shapes are in:

- `crates/spur-telemetry/src/tier1_events.rs`
- `crates/spur-telemetry/src/tier2_events.rs`
- `crates/spur-telemetry/src/crash.rs`

Examples of emitted categories include model class, duration, token bucket, outcome status, tool/server category, and sanitized crash diagnostics (such as hashed payload references and sanitized stacks). Verified schema review indicates prompt bodies and source-code content are not serialized as telemetry payload fields.

## 4) Local Execution Safety

SPUR is designed to operate inside the user's existing OS and repository permission boundary rather than introducing a separate SPUR-hosted execution plane. Worker tasks are executed in git worktree-based isolation under `.spur/worktrees`, which reduces direct contamination risk to the primary checkout during agent execution.

For high-impact actions, SPUR includes explicit confirmation patterns in current code paths (for example, confirmation gating for destructive plan-ownership force-reclaim operations, and interactive cancel-confirm behavior in TUI session flow) instead of silently executing potentially disruptive operations from a single accidental keypress or call.

This section should be interpreted as an implementation-scope statement, not a formal sandbox guarantee: SPUR improves operational safety using local isolation and confirmations, but still runs with the effective filesystem/process privileges of the local user account and host environment.

## 5) Supply Chain

SPUR maintains a repository security workflow at [`.github/workflows/security-scans.yml`](../../.github/workflows/security-scans.yml). In its current form, it runs:

- `cargo-audit` (RustSec advisory checks)
- `cargo-deny` (advisories/licenses/bans/sources policy checks)
- `gitleaks` scans (working tree on pull requests; full-history baseline on schedule)

Results are exported as CI artifacts for review. This overview does not claim external certification (for example SOC 2 or ISO 27001).

Software bill of materials (SBOM) output can be provided to enterprise buyers on request.

## 6) Vulnerability Reporting

Report suspected vulnerabilities or security concerns to:

`security@<TODO-domain>`

Use encrypted email when possible and include reproducibility details (version, platform, reproduction steps, and impact assessment) to speed triage.
