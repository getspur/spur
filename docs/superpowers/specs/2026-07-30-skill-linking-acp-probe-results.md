# Skill Linking ACP Probe Results

**Probe date:** 2026-07-30
**Design epic:** `bd-1vnls`
**Related design:** [standards-first workflow linking design](2026-07-30-standards-first-skill-workflow-linking-design.md)
**Probe harness:** [`scripts/probe_acp_capabilities.py`](../../../scripts/probe_acp_capabilities.py)

## Question

Can standards-compliant project skills express a SPUR workflow relationship
that Codex and OpenCode discover and follow, and what must SPUR compile for
that relationship to work after adapter name prefixing?

## Environment

| Component | Version |
|---|---|
| Codex CLI | `0.144.6` |
| Codex ACP adapter | `@agentclientprotocol/codex-acp 1.1.2` |
| OpenCode | `1.17.9` |
| Python | `3.12.6` |

The probe used a disposable git repository with four ordinary Agent Skills
under `.agents/skills/`. No host-specific sidecar or `skill-graph.toml` was
present. Each fixture used only standard top-level frontmatter:

```yaml
---
name: spurpower-link-probe-rewritten-alpha
description: Run the positive linked-skill probe.
metadata:
  "getspur.schema": "workflow/v1"
  "getspur.role": "both"
  "getspur.requires": "spurpower-link-probe-beta"
---
```

The beta skill contained the otherwise-hidden token
`BETA_EVIDENCE_7Q9M2K`. A successful result therefore required the agent to
load the beta body; the alpha description and prompt did not contain the
token.

## Cases

| Case | Relationship/body behavior | Expected evidence |
|---|---|---|
| Discovery | Initial three fixture skills present at session creation | All three advertised |
| Metadata only | Rewritten `getspur.requires`; alpha forbids loading beta | Metadata tolerated but beta not preloaded |
| Prefix mismatch | Metadata/body requests canonical `link-probe-beta`, while only `spurpower-link-probe-beta` exists | Exact target unavailable |
| Rewritten target | Metadata/body requests visible `spurpower-link-probe-beta` | Beta loads and returns hidden token |

The live invocations used the existing raw ACP harness:

```bash
python3 scripts/probe_acp_capabilities.py \
  --command codex-acp \
  --args "" \
  --cwd "$PROBE_REPO" \
  --no-try-set \
  --always-approve \
  --prompt '$spurpower-link-probe-rewritten-alpha Run SPUR_LINK_PROBE_REWRITTEN.'

python3 scripts/probe_acp_capabilities.py \
  --command opencode \
  --args acp \
  --cwd "$PROBE_REPO" \
  --no-try-set \
  --always-approve \
  --prompt 'Use the skill named spurpower-link-probe-rewritten-alpha to run SPUR_LINK_PROBE_REWRITTEN.'
```

Handshake-only and prompt runs wrote separate JSONL traces and report files.
The raw artifacts remained in the disposable `.spur/scratch/` repository
because they include the machine's complete discovered skill catalog and local
paths.

## Results

| Observation | Codex ACP | OpenCode ACP |
|---|---|---|
| Discovers `.agents/skills` fixture | Yes | Yes |
| Accepts namespaced string metadata | Yes | Yes |
| Advertised invocation name | `$spurpower-…` | `spurpower-…` |
| Automatically interprets `getspur.requires` | No | No |
| Unrewritten `link-probe-beta` resolves | No | No |
| Rewritten `spurpower-link-probe-beta` resolves | Yes | Yes |
| Hidden beta token returned | Yes | Yes |
| Observed nested activation | Reads exact projected `SKILL.md` through file/shell tooling | Calls native `skill` tool with exact projected name |

Exact result lines were:

```text
Codex:    SPUR_LINK_PROBE_METADATA_ONLY beta=not-preloaded
OpenCode: SPUR_LINK_PROBE_METADATA_ONLY beta=not-preloaded

Codex:    SPUR_LINK_PROBE_MISMATCH beta=unavailable
OpenCode: SPUR_LINK_PROBE_MISMATCH beta=unavailable

Codex:    SPUR_LINK_PROBE_REWRITTEN beta=BETA_EVIDENCE_7Q9M2K
OpenCode: SPUR_LINK_PROBE_REWRITTEN beta=BETA_EVIDENCE_7Q9M2K
```

OpenCode's negative trace contains a failed native call with
`rawInput.name = "link-probe-beta"` and lists
`spurpower-link-probe-beta` as available. Its positive trace contains
completed `skill` calls for both alpha and beta.

Codex's negative trace reads only the invoked alpha and returns unavailable.
Its positive trace reads the alpha and beta `SKILL.md` files using their exact
projected names. Codex ACP did not expose a nested `skill` RPC analogous to
OpenCode's.

The handshake catalog also demonstrates the description-pressure problem. With
three probe fixtures present, Codex advertised 62 commands containing 19,352
description characters; OpenCode advertised 51 commands containing 17,837
description characters. A graph sidecar would not reduce either catalog.

## Design Verdict

The standards-first proposal passes with one important refinement:
availability, activation, and enforcement are separate layers.

1. Standard `SKILL.md` plus namespaced string metadata is a valid portable
   source format.
2. Native hosts tolerate the SPUR metadata but do not execute it. SPUR must
   parse the graph and compute the workflow closure.
3. Projection must rewrite every relationship endpoint to the exact
   host-visible name. Leaving canonical names in the projected body is a real
   failure on both tested hosts.
4. Merely projecting the dependency makes it available, not active. SPUR must
   compile an adapter-specific activation agenda for the launch prompt, and
   projected workflow blocks must contain imperative activation instructions.
5. Integration tests must verify target-body evidence rather than require one
   universal tool-call shape. OpenCode and Codex activate linked skills
   differently.
6. Runtime narrowing remains necessary for the original warning. Only
   role/workflow closure reduces the descriptions exposed to a session.

## Limitations

- The probe covers the installed Codex ACP and OpenCode versions, not every
  release or Claude Code.
- Skill activation remains model-mediated. The successful token proves body
  loading in these runs, not a vendor-level dependency guarantee.
- Codex's nested file/shell behavior is observed behavior, not a portable ACP
  protocol method.
- Production acceptance still requires hermetic adapter integration tests and
  a capability probe when an adapter version changes.
