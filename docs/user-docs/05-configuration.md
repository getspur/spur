# Configuration System

Spur layers built-in defaults, user configuration from `~/.spur/config.toml`, and repository configuration from `<repo>/.spur/config.toml`, in that order. Later layers override earlier layers, so a repository can override a user-wide setting. Tables are merged recursively.

When you run `spur init`, Spur scans your system for supported agents and generates `.spur/config.toml` in the repository root. This guide explains the effective configuration and how you can customize it to add agents, change how the "Brain" delegates tasks, configure permissions, and opt into the skills catalog runtime.

## The `.spur/config.toml` File

Repository-specific settings live in `.spur/config.toml`; user-wide defaults can live in `~/.spur/config.toml`. Repository values take precedence when both layers define the same setting.

*Note: Re-running `spur init` will overwrite the repository file with a new seed template, so it is recommended to edit it by hand once generated.*

### 1. Brain Framework Configuration

At the top of the file, you can configure how the "Brain" agent orchestrates tasks.

```toml
[brain.delegation]
# "v1" enables the advanced brain prompt (workers block, dispatch procedure).
# "legacy" uses the simpler, pre-framework prompt.
framework = "v1"
```

### 2. Agent Entries (`[[agents.entries]]`)

The core of the config file is the `agents.entries` array. Each entry defines an AI agent that Spur can communicate with. 

Here is an example of an agent configuration:

```toml
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
role = "both"
cost_tier = "medium"
```

#### Key Fields:
*   **`name`**: A unique identifier for the agent in the config.
*   **`command`**: The executable command used to launch the agent (e.g., `npx`, `kiro-cli`, `codex`).
*   **`args`**: Command-line arguments passed to the agent on startup.
*   **`transport`**: How Spur communicates with the agent. 
    *   `acp`: Uses the standard Agent Client Protocol (JSON-RPC 2.0).
    *   `stream-json`: For tools that output streaming JSON but aren't strictly ACP.
    *   `cli-wrap`: Wraps standard CLI stdin/stdout tools.
*   **`role`**: Defines what this agent is allowed to do. 
    *   `brain`: Only acts as an orchestrator.
    *   `worker`: Only acts as an executor.
    *   `both`: Can be used as either.
*   **`cost_tier`**: Used by the Brain to make economic delegation choices (`low`, `medium`, `high`).

### 3. Display and Dispatch Settings

You can customize how the agent appears in the UI and how it receives commands.

```toml
[agents.entries.display]
handle = "claude" # Used for @mentions (e.g., @claude)

[agents.entries.commands]
dispatch = "prompt_text" # How the initial prompt is sent
```

### 4. Permissions and Auto-Approval

By default, many AI agents require user confirmation before running terminal commands or modifying files. You can configure Spur to automatically pass bypass flags.

```toml
[agents.entries.permissions]
# Example for Claude Code ACP bypass
session_mode = "bypassPermissions"

# Example for standard CLI bypass
# args = ["--dangerously-skip-permissions"]
# skip = true
```

### 5. Brain Delegation tuning

You can change how the Brain perceives a specific worker agent. By overriding the delegation descriptor, you tell the Brain what an agent is "good for" and what it should "avoid".

```toml
[agents.entries.delegation]
description = "A fast, inexpensive agent for simple refactoring."
good_for = ["Writing tests", "Updating documentation", "Simple refactors"]
avoid_for = ["Complex architectural changes", "Database migrations"]
```
*(If omitted, Spur uses built-in defaults for known agents).*

## Graph embedding model

Code-graph and analyst hybrid search use a selectable FP32/768 embedding model. Set it in config instead of exporting `SPUR_EMBEDDING_MODEL`:

```toml
[graph]
# nomic (default), coderank, or jina-code
embedding_model = "nomic"
```

Repository `.spur/config.toml` wins over `~/.spur/config.toml`. If `SPUR_EMBEDDING_MODEL` is set in the environment, that value still overrides the config file.

## Brain Skills

Spur ships with built-in `SKILL.md` files that instruct the Brain on how to delegate, review, and coordinate tasks. 

If you want to override these instructions for a specific project:
1. Create `.spur/skills/brain-delegation-{agent}/SKILL.md`.
2. Add your custom system prompts or procedures.
3. The Brain will prioritize your project-specific skill file over the built-in defaults.

## Skills Catalog MCP Rollout

The skills catalog runtime is the **default** projection for brain, worker, and init sessions. When `skills.projection_mode` is absent, Spur uses `catalog_only`: only **bundled foundation skills** are materialized into adapter skill directories. All other skills are discovered and loaded through the catalog MCP (`skill_navigate` / `skill_read`), not by pre-installing them on disk.

**Foundation set** (bundled only; pool/overrides excluded):  
`skills-catalog`, `spur-way`, `code-explore`, `solve`, `brain-delegation`, `brain-review-gate`, `plan-task-discipline`, `worker-signals`, `beads-lifecycle`, `spur-analyst`

The bootstrap skill `skills-catalog` is required. A missing bootstrap, a symlink in its place, invalid frontmatter, or another integrity failure stops projection instead of falling back to unverified content. Other foundation IDs are included when present in the bundled root.

Legacy full materialization remains available as an explicit rollback: `projection_mode = "all_active"`.

### Layered configuration

The effective precedence is:

1. built-in default: `catalog_only` (foundation preload);
2. user setting: `~/.spur/config.toml`;
3. repository setting: `<repo>/.spur/config.toml`.

To force the legacy full skill materialization, add this to the appropriate config file:

```toml
[skills]
projection_mode = "all_active"
```

To pin foundation/catalog mode explicitly (same as the default):

```toml
[skills]
projection_mode = "catalog_only"
```

A repository value wins over the user value. The only accepted values are `"all_active"` and `"catalog_only"`.

The effective setting is read when Spur reconciles a new brain, worker, or init projection. Start a new session after changing it. Existing conversations retain skills and retrieved text already delivered to their context.

### One-bootstrap navigate/read flow

The `skills-catalog` bootstrap uses a bounded, repeated PageIndex discovery flow:

1. Call `skill_navigate` with a natural-language `query` for the current task intent.
2. Inspect hits (skill metadata, node kind/path/heading, optional lede). Optionally call `skill_navigate` again with `root` set to a returned `skill_id` or `skill_id:node_id` to expand one tree hop.
3. Choose a relevant hit and copy its opaque `skill_id` unchanged.
4. Call `skill_read` with that exact `skill_id` to load `SKILL.md`.
5. If the loaded skill explicitly needs an approved text resource, call `skill_read` again with the same `skill_id` and the declared relative `resource` path.
6. Navigate again when the task changes phase or the first query is insufficient.

If the catalog MCP is unavailable, the bootstrap directs the agent to continue with base-agent capabilities or report that no approved workflow could be loaded. It must not search the filesystem, walk skill directories, fabricate an ID, or install a task-specific skill.

#### PageIndex corpus

`skill_navigate` searches and tree-hops an in-memory **PageIndex** built only for currently eligible skills. The index combines:

1. **Frontmatter metadata** — parsed YAML fields such as name, description, and role
2. **SKILL.md headings and section bodies** — after the frontmatter block is stripped
3. **Approved text resources** — inventory-approved UTF-8 text only (scripts, binaries, and undeclared paths are excluded)

Navigate hits are **metadata + lede only** (lede defaults to the first ~200 UTF-8 characters of the node’s searchable text). Full instruction or resource bodies are never returned by navigate or search; they are available only through `skill_read`.

#### `skill_navigate`

`skill_navigate` accepts this schema:

```json
{
  "query": "validate authentication changes before merging",
  "root": null,
  "limit": 5,
  "source": null,
  "include_lede": true
}
```

- `query` is required when `root` is omitted or empty; it is full-text BM25 over PageIndex node tokens.
- `root`, when set, is an opaque `skill_id` or `skill_id:node_id`. The tool expands **one tree hop** (children of that node) instead of running FTS.
- `limit` is optional, defaults to `5`, and must be an integer from `1` through `5`.
- `source` is optional and, when set, is an exact provenance filter (FTS path only; same semantics as `skill_search`).
- `include_lede` is optional and defaults to `true`. When `false`, lede fields are omitted from hits.
- Unknown fields and wrong field types are rejected. An empty call (no non-empty `query` and no non-empty `root`) fails with `invalid_query`.

A successful response contains `catalog_revision` and `hits`. Each hit includes `skill_id`, `node_id`, `name`, `node_kind` (`frontmatter`, `document`, `section`, or `resource`), `path`, `heading`, `heading_level`, `child_count`, optional FTS `score`, optional `lede`, `source`, `availability`, and `rank`. Hits never contain full instruction bodies and never write to the worker filesystem (`write_effect = "none"`).

#### `skill_search`

`skill_search` remains available as skill-level, metadata-only discovery (no PageIndex nodes). It accepts this schema:

```json
{
  "query": "validate authentication changes before merging",
  "limit": 5,
  "source": null
}
```

- `query` is required and must contain non-empty task intent.
- `limit` is optional, defaults to `5`, and must be an integer from `1` through `5`.
- `source` is optional and, when set, is an exact provenance filter.
- Unknown fields and wrong field types are rejected.

Search is metadata-only. Its top-level response contains `catalog_revision` and `results`. Each result contains `skill_id`, `name`, `description`, `source`, `pinned_commit`, `content_sha256`, `resource_manifest_sha256`, `compatibility`, `availability`, `rank`, and `match_reason`; it never contains the instruction body.

#### `skill_read`

`skill_read` accepts this schema:

```json
{
  "skill_id": "opaque-versioned-reference-from-navigate-or-search",
  "resource": null
}
```

- `skill_id` is required, non-empty, opaque, and must be copied from navigate or search rather than parsed or constructed.
- Omit `resource`, set it to `null`, or use `"SKILL.md"` to read the main instructions.
- Otherwise, `resource` must be a normalized relative path in the approved text-resource inventory.
- Unknown fields and wrong field types are rejected.

A successful read returns `skill_id`, `name`, `source`, `catalog_revision`, `content_sha256`, `resource`, `media_type`, and exact `content`.

### Provenance, integrity, and context-only delivery

`source` and `pinned_commit` identify provenance. `content_sha256` identifies the pinned skill content, `resource_manifest_sha256` identifies the approved text-resource inventory, and `catalog_revision` identifies the merged eligible catalog and policy view used for the response. Treat `skill_id` as an opaque, version-pinned reference; unrelated catalog changes do not authorize altering it.

A navigate or search result is not an authorization capability. Before every read, Spur reloads current catalog state and rechecks the opaque reference, current eligibility, context compatibility, version identity, requested resource, and content integrity. A removed, disabled, unapproved, changed, or no-longer-compatible result fails closed. Eligibility is `enabled AND compatible AND (bundled OR (adopted AND gate-approved))`.

Delivery is context-only:

- `skill_navigate`, `skill_search`, and `skill_read` have `write_effect = "none"` and do not install or materialize task-specific skills in the worker filesystem.
- Navigate and search return metadata and optional ledes only; only `skill_read` returns full verified UTF-8 skill or resource text in the MCP result.
- Scripts, binary resources, non-UTF-8 files, symlinks, undeclared resources, unsupported media, absolute paths, traversal, and cross-skill paths are denied.
- Text is limited to `262144` bytes by `MAX_TEXT_CONTENT_BYTES`, recorded by persisted solve result `sol_ece03f4a166e4004`.
- A resource is checked for type, size, canonical containment, and SHA-256 immediately before return; the whole skill content hash is rechecked after the resource read.
- Retrieved instructions remain below system, developer, user, repository, and project-management authority.

### Stable errors

Catalog errors include JSON-RPC `data.error_kind` and `data.write_effect = "none"`. Invalid input uses JSON-RPC code `-32602`; the catalog domain errors below use `-32004`.

| `error_kind` | Meaning | Operator or agent response |
|---|---|---|
| `invalid_query` | Arguments are malformed, unknown, empty, out of range, or the wrong type. | Correct the arguments; do not infer partial success. |
| `skill_not_found` | The opaque reference is unknown. | Discard it and search again. |
| `skill_not_eligible` | The skill or catalog is unavailable, disabled, unapproved, removed, or incompatible. | Search again; do not bypass governance. |
| `stale_skill_ref` | The referenced version is no longer current. | Discard it and search again. |
| `resource_not_found` | The resource is absent from the approved text inventory or disappeared. | Read `SKILL.md` or another resource explicitly declared by the skill. |
| `resource_denied` | The path, file type, media type, encoding, or containment is unsafe. | Stop the resource attempt; do not construct another filesystem path. |
| `content_too_large` | Skill or resource text exceeds the context-only size policy. | Report the catalog-policy problem and use base capabilities if possible. |
| `integrity_mismatch` | Size, resource hash, pinned content hash, or a read-time recheck failed. | Fail closed and investigate the catalog source. |

An unrooted registry call fails with JSON-RPC code `-32001` and `error_kind = "authority_root_required"`; catalog tools require a repository authority root. Unexpected `internal_error` failures are operational faults, not permission to use partial content.

### Observe the opt-in

Catalog completions are `info`, failures are `warn`, and the search-start event is `debug`. To capture the complete flow, add the module directive to the effective config:

```toml
[log]
level = "warn,spur_core::orchestrator=info,spur_core::mcp::skills_catalog=debug"
```

`RUST_LOG` overrides `[log].level`; use the same `spur_core::mcp::skills_catalog=debug` directive in the launch environment when appropriate.

| Event | Evidence fields |
|---|---|
| `skill_navigate_started` | `tool`, exact `source` filter, `has_root`, `has_query`, `include_lede`, `write_effect` |
| `skill_navigate_completed` | `tool`, `source`, `catalog_revision`, `result_count`, `include_lede`, `latency_ms`, `write_effect` |
| `skill_navigate_failed` | `tool`, `source`, `catalog_revision`, `result_count`, `error_kind`, `latency_ms`, `write_effect` |
| `skill_search_started` | `tool`, exact `source` filter, `write_effect` |
| `skill_search_completed` | `tool`, `source`, `catalog_revision`, `result_count`, `latency_ms`, `write_effect` |
| `skill_search_failed` | `tool`, `source`, `catalog_revision`, `result_count`, `error_kind`, `latency_ms`, `write_effect` |
| `skill_read_completed` | `tool`, `source`, opaque `skill_id`, `catalog_revision`, `content_sha256`, `resource`, `result_count`, `latency_ms`, `write_effect` |
| `skill_read_failed` | `tool`, `source`, `catalog_revision`, `result_count`, `error_kind`, `latency_ms`, `write_effect` |

Raw navigate/search queries and returned skill content are not logged by these events. Preserve event counts, latency distributions, stable-error rates, stale/denied read rates, skills read per task, startup and cumulative skill-token use, and downstream task outcomes for the observation window.

### Four-gate rollout decision

The executable `CATALOG-ROLLOUT-GATE` report is `d6b1e57cf466b9750c6ecfbf22ac513890c002e4d1e06e237977e5c5a8913082`. It proves the four-input decision is deterministic, covers all input combinations, has mutually exclusive outcomes, and has witnesses for both outcomes. It does not prove the Rust implementation or retrieval quality.

Collect and retain evidence for all four gates:

| Gate | Required evidence |
|---|---|
| Retrieval | Deterministic fixture results for recall@5, precision@5, mean reciprocal rank, zero-result rate, and refinement recovery, compared with the approved frozen baseline; include activation/no-match precision and downstream task outcomes before changing defaults. |
| Security | Fresh tests for eligibility, revocation, stale references, unapproved shadowing, resource confinement, content-size policy, integrity mismatch, and unchanged worker files; runtime failures must remain fail-closed with `write_effect = "none"`. |
| Integration | Fresh rooted brain and worker registry navigate/search/read tests, exact-one catalog-only projection evidence, MCP-unavailable fallback evidence, and an exercised `all_active` rollback. |
| Observation | An approved observation window showing acceptable navigate/search/read latency and error rates, lower startup skill-token use, catalog-churn behavior, skills-read accumulation, and no meaningful downstream task regression. |

If operational gates fail after a foundation/catalog default rollout, restore `all_active` via layered config (see Roll back). The `all_active` path remains implemented and is the supported full-materialization rollback.

The design evidence also includes persisted solve result `sol_46039afe656a4dff` (`sat`, showing the one-bootstrap, approval-gated, lexical, repeated-search, context-only design is feasible) and `sol_b57f4c1096ef4a0c` (`unsat`, excluding an unapproved successful read or a task-specific filesystem write under the encoded policy). The first result's `search_result_limit = 3` is a feasibility witness, not the API contract; the implemented default and maximum are both `5`. Solver and NS-Mermaid evidence validate encoded contracts and do not replace fresh tests or operational measurements.

Before a rollout decision, attach fresh output from the configuration, projection, serving, MCP schema/catalog, and integration checks:

```bash
scripts/spur-cargo test -p spur-acp config
scripts/spur-cargo test -p spur-core skills::projection
scripts/spur-cargo test -p spur-core explore::serving
scripts/spur-cargo test -p spur-core mcp::skills_catalog
scripts/spur-cargo test -p spur-core --test tool_schema_stability
scripts/spur-cargo test -p spur-core --test tool_catalog
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
```

### Roll back to full materialization

Set the most specific applicable layer to `all_active`. For a repository, change `<repo>/.spur/config.toml`:

```toml
[skills]
projection_mode = "all_active"
```

An explicit project value is safer than merely deleting the key when `~/.spur/config.toml` might still set foundation mode. For a user-wide rollback, set `all_active` in `~/.spur/config.toml` and also change any repositories whose project file still sets `catalog_only`, because project configuration wins.

Start new brain and worker sessions after the change. Confirm the effective layer, observe that the new runtime uses the legacy all-active projection, and retain the catalog traces and failure evidence that triggered rollback. Existing conversations retain context already delivered to them; rollback does not erase prior model context.
