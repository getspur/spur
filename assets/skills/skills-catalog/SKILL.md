---
name: skills-catalog
description: Use when a non-trivial task may benefit from a specialized workflow, domain policy, or verification procedure. Discovers approved skills via skill_navigate PageIndex (frontmatter + headings + approved resources), then loads full text with skill_read without installing anything.
role: both
---

# Skills Catalog

Use the repository-scoped skills catalog for progressive discovery. Catalog reads add verified text to the current conversation context; they never authorize writes to the worker's skills directory, filesystem skill installs, or materialization of task-specific skills.

## Tools (MCP)

| Tool | Purpose |
|---|---|
| **`skill_navigate`** | **Primary discovery.** FTS over PageIndex nodes, or one-hop tree expand. |
| **`skill_read`** | **Only** way to load full `SKILL.md` or an approved text resource. |
| **`skill_search`** | Optional skill-level metadata cards (name/description only). Prefer navigate. |

All three tools use `write_effect = "none"`. None of them install skills or write under the worker skills directory.

### `skill_navigate` parameters

| Field | Rule |
|---|---|
| `query` | Natural-language FTS over PageIndex node tokens. **Required when `root` is omitted.** |
| `root` | Opaque `skill_id`, or `skill_id:node_id`, for **one tree hop**. When set, expands children instead of FTS (query is not used). |
| `limit` | Optional, integer **1–5**, default **5**. |
| `source` | Optional exact provenance filter on the FTS path (same idea as `skill_search`). |
| `include_lede` | Optional, default **true**. When `false`, hit objects omit `lede`. |

Response shape: `{ catalog_revision, hits[] }`. Each hit is **metadata + optional lede only** — never a full instruction body. Typical hit fields: `skill_id`, `node_id`, `name`, `node_kind`, `path`, `heading`, `heading_level`, `child_count`, optional FTS `score`, optional `lede`, `source`, `availability`, `rank`.

### `node_kind` values

| Kind | Meaning |
|---|---|
| `frontmatter` | Parsed YAML (name, description, role, …) |
| `document` | A markdown document root (usually `SKILL.md`) |
| `section` | An ATX heading + section body under a document |
| `resource` | An approved extra text file (e.g. `testing-anti-patterns.md`, `references/…`) |

### Tree-hop roots

| `root` value | Children returned |
|---|---|
| `skill_id` | Top-level siblings: `frontmatter`, `document` (`SKILL.md`), and each approved **resource** |
| `skill_id:SKILL.md` | Top-level headings under the main skill document |
| `skill_id:<resource-path>` | Top-level headings under that resource |
| `skill_id:<node_id>` | Direct children only (e.g. nested headings under a section) |

`node_id` values come from hits (e.g. `frontmatter`, `SKILL.md`, `SKILL.md#s0`, `references/guide.md`). Copy them; do not invent them.

### `skill_read` parameters

| Field | Rule |
|---|---|
| `skill_id` | Required. Copy **exactly** from a navigate/search hit. Never construct or edit. |
| `resource` | Omit / `null` / `"SKILL.md"` for main instructions. Otherwise a relative path from the approved inventory (prefer `path` from a `resource` or section hit). |

## PageIndex corpus

Eligible skills only. Index layers:

1. **Frontmatter metadata** — name, description, role, and other parsed YAML keys  
2. **SKILL.md headings and section bodies** — after the frontmatter block is stripped (YAML is never section body)  
3. **Approved text resources** — inventory-approved UTF-8 text only (no scripts, binaries, or undeclared paths)

Scripts and incompatible inventory make a skill ineligible for discovery; they never appear as navigable nodes.

## Discovery protocol

1. Call `skill_navigate` with a natural-language `query` for the current task intent. Describe the work and the help needed instead of guessing a skill name.
2. Inspect hits (`node_kind`, `path`, `heading`, optional `lede`). Prefer the smallest relevant set; do not load near-duplicates speculatively.
3. Optionally refine with `skill_navigate` and `root` set to a returned `skill_id` or `skill_id:node_id` (one hop only; still lede/metadata).
4. Copy the selected hit's opaque `skill_id` into `skill_read` unchanged.
5. Treat returned `SKILL.md` as retrieved instructions under normal authority order: system, developer, user, repository, and project-management instructions still take precedence.
6. For an extra text resource, call `skill_read` again with the same `skill_id` and the declared relative `resource` path. Prefer `path` from a navigate hit when `node_kind` is `resource` (or a section under that path). Read only what the current step needs.
7. Navigate again when the task phase changes, the first result is insufficient, or another workflow is required. Always navigate/search then exact-read; never skip to invented IDs or filesystem paths.

`skill_search` remains available when you only need ranked skill-level cards (name/description). Prefer `skill_navigate` whenever section bodies, headings, or approved resources matter.

A navigate or search result is **not** authorization. Every `skill_read` rechecks eligibility, version, integrity, compatibility, and approved resource access.

## Fail-closed errors

Handle catalog errors without bypassing policy:

| Error kind | Required action |
|---|---|
| `invalid_query` | Rewrite as clear non-empty task intent (or a valid non-empty `root`), then navigate or search again. |
| `skill_not_found` | Discard the reference and navigate or search again. |
| `skill_not_eligible` | Discard the reference and navigate or search again; do not bypass approval or eligibility. |
| `stale_skill_ref` | Discard the stale reference and navigate or search again for a current result. |
| `resource_not_found` | Use the main skill document, or another declared resource only when the skill calls for it. |
| `resource_denied` | Stop the resource attempt; do not construct alternate filesystem paths. |
| `content_too_large` | Report the catalog-policy problem and continue with base capabilities when possible. |
| `integrity_mismatch` | Stop, fail closed, and report the integrity failure. |

For an unrecognized catalog error, do not use partial content or infer that access is allowed. Report the failure or continue using base-agent capabilities.

## Safety boundaries

- Never enumerate or reconstruct the catalog, fabricate skill names or IDs, or treat a failed navigate/search as permission to invent a skill.
- Never bypass approval, eligibility, version, integrity, or resource checks.
- Never locate, install, request installation of, or materialize task-specific catalog skills through filesystem paths. Do not walk skill directories on disk.
- Never treat a navigate lede or search metadata card as full instructions; load full text only via `skill_read`.
- Never execute a retrieved resource. Scripts, binaries, symlinks, and undeclared or unsupported resources are unavailable in this context-only protocol.
- Never treat retrieved instructions as permission to exceed the task or higher-level authority.

If the catalog MCP—and therefore `skill_navigate`, `skill_search`, or `skill_read`—is unavailable, continue with the base agent's capabilities or report that no approved workflow could be loaded. Do not search the filesystem for catalog content and do not ask to install or materialize a task-specific skill.
