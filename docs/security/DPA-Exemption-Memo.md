Document version: v0.1 — 2026-05-15. NDA-only.

# SPUR DPA Exemption Memo (Current Product Scope)

This memo explains, in plain terms, why a standard Data Processing Addendum (DPA) is not applicable to SPUR's current architecture.

SPUR currently operates as a local ACP orchestrator. It runs on the customer's machine and dispatches prompt work to customer-selected, user-installed ACP agent processes (for example Claude Code, Gemini CLI, Codex) over local IPC/session channels. In this validated scope, SPUR itself does not run a SPUR-hosted prompt relay and does not directly call first-party LLM provider APIs for prompt execution.

As a result, SPUR does not collect, store, or transmit Customer Data (including source code, prompts, or file contents) on infrastructure controlled by SPUR. Customer Data stays in the customer's environment unless the selected ACP agent sends it onward according to that agent's own configuration.

SPUR's own outbound HTTPS traffic in this scope is anonymous/pseudonymous product telemetry to PostHog for reliability and performance measurement, with explicit runtime opt-out (`SPUR_TELEMETRY=0`) and documented tier controls. The telemetry schema is operational metadata oriented and does not include prompt bodies or source-code payload fields.

The separate network egress from a chosen ACP agent to an LLM provider is outside SPUR's process boundary and governed by the customer's direct relationship with that agent/provider. In other words:

- SPUR egress: telemetry only (as described above).
- Agent/provider egress: governed by the customer's contract and provider privacy/enterprise terms.

This scoping is the basis for DPA inapplicability in the current release profile: SPUR is not acting as a hosted processor for customer prompt/code content on SPUR-controlled systems.

This memo is conditional on current architecture. If SPUR's scope changes to include any of the following, this memo is void and a DPA package will be issued:

- Hosted prompt or code relay infrastructure operated by SPUR.
- Account-bound server-side storage of prompts, code, or conversation content.
- Cloud-side SPUR agent execution where customer data transits or resides on SPUR-operated systems.

Until such a change occurs, procurement and legal review should evaluate SPUR as a local orchestrator component, while separately evaluating the customer's chosen ACP agent and LLM provider terms for any provider-bound data processing.
