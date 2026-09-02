# ACP capability probe and fixture contract

`scripts/probe_acp_capabilities.py` keeps its legacy report fields and adds an
`artifact` object. The contract is additive and is identified by:

```json
{"schema": "spur.acp-capability-probe", "version": 1}
```

Consumers must ignore artifacts with an unknown schema or version rather than
partially interpreting them.

## CLI identity

`artifact.cli_identity` binds evidence to the resolved executable, upstream
version, SHA-256 fingerprint of the redacted argv, and SHA-256 fingerprint of
the non-secret `LANG`, `LC_ALL`, `LC_CTYPE`, `PATH`, and `SHELL` environment
inputs. Environment values are never written to the report. Credential-bearing
argv values are replaced with `<redacted>` before fingerprinting.

## Raw evidence

`artifact.raw.frames` is the ordered ledger captured by the ACP client before
typed projection. Every entry has a zero-based `sequence`, a `send` or `recv`
direction, the complete sanitized JSON-RPC `message`, and its `sha256:<hex>`
digest reference into `payloads_by_digest`. Sequence is assigned at capture
time and is independent of wall-clock timestamps, so requests, responses,
notifications, and server requests retain their true interleaving.

Explicit probe outcomes with no response frame, such as a timeout or transport
failure, live in `artifact.raw.operational_outcomes`; they are never inserted
into the protocol frame order. Stderr and unparsed stdout remain JSONL
diagnostics and are not protocol evidence. All retained payloads are
recursively redacted before hashing, so digests identify replayable sanitized
evidence and never a secret-bearing original.

## Claims

Every `artifact.claims` entry contains a semantic capability `{kind, id}`, a
claim, provenance, source, observation time, raw digest, and optional session
scope. The version-1 claim/provenance pairs are:

| Observation | `claim` | `provenance` |
|---|---|---|
| Standard payload advertises a choice | `advertised` | `standard_advertisement` |
| Vendor payload advertises a choice | `advertised` | `vendor_advertisement` |
| Active method succeeds | `accepted` | `accepted_active_probe` |
| Active method explicitly rejects | `rejected` | `rejected_active_probe` |
| Notification arrives | `observed` | `observed_notification` |
| Prompt fallback succeeds | `prompt_fallback` | `prompt_fallback` |
| Authentication, timeout, transport, or probe failure | `inconclusive` | `inconclusive_failure` |

Recipe metadata such as `optionsMethod` is not a claim. A recipe method becomes
evidence only after a response or failure is observed. Model, effort, mode,
command, and returned option IDs are copied from payloads; no provider or value
allowlist participates in claim construction.

## Deterministic fixture material

`artifact.fixture` has schema `spur.acp-capability-fixture`, version `1`, and
contains the CLI identity, sanitized raw ledger, and normalized claims. It
omits per-run claim timestamps and carries a digest over the remaining
canonical JSON. Given the same captured frames and identity, fixture bytes and
digest are stable regardless of probe time. Checked-in fixtures should be the
`artifact.fixture` object, serialized with sorted keys and a trailing newline.

The fixture is evidence, not a support manifest. Routing and support decisions
belong to the evidence reducer; they must not be inferred from recipe presence.
