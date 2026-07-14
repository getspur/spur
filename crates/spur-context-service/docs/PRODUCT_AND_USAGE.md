# SPUR Context Service — Product & Usage

Business value and practical how-to for humans and agents. For internals
(medallion layers, DynamoDB/S3/Lambda), see [ARCHITECTURE.md](./ARCHITECTURE.md).

---

## 1. What it is

**SPUR Context Service** is cloud **code context for third-party packages**.

It indexes a package revision (`source` + `package` + `revision`) into a code
graph, then exposes MCP tools so coding agents can:

- search symbols by name,
- read real source at a pinned version,
- walk callers and callees inside that package,
- ask natural-language questions over indexed code and docs.

It is the **external** counterpart to worktree tools (`knowledge_context_pack_2`,
`code_*`). Local tools answer “what’s in *this* repo?”; context service answers
“what’s in *dependency X at version Y*?”

| Plane | Question | Tools |
|---|---|---|
| Worktree | This repository | `knowledge_context_pack_2`, `code_*` |
| External | Dependencies / upstream packages | `external_*` (this service) |

Default public origin (CLI default when URL is omitted):
`https://context.getspur.dev`. Override with `--url`,
`SPUR_CONTEXT_SERVICE_URL`, or `[context_service].url` in config.

---

## 2. Business value

### Problem

Agents and developers are strong on in-repo code (local graph, IDE, SPUR
worktree tools) but weak on **dependencies**. Without structured external
context they fall back to:

- web search and outdated docs,
- documentation-only indexes (no call graph / real implementation),
- guessing APIs across versions.

Wrong dependency assumptions cause broken integrations, missed breaking
changes, and rework.

### Value

| Stakeholder | Outcome |
|---|---|
| **Developers** | Version-precise answers (“how does `serde@1.0.197` implement X?”) without cloning every crate |
| **Coding agents** | Same graph-first workflow outside the worktree; fewer hallucinated APIs |
| **Teams** | Shared index of public packages; on-demand builds instead of always-on heavy catalogs |
| **Product** | Differentiator: **code structure as a service**, not “search the internet” |
| **Ops / cost** | Warm hits skip rebuilds; serving reads frozen snapshots (no Postgres on the read path); quotas and abuse controls bound spend |

### Strategic fit inside SPUR

- Complements local intelligence; does not replace worktree `code_*`.
- MCP-native — drops into existing agent tool loops.
- On-demand long-tail indexing (fetchable git or tarball), not only a fixed allow-list.
- Auth modes for personal keys, org M2M, and internal AWS IAM.

### Explicit non-goals

- Not a host for private monorepos (shared catalog is for **public** packages).
- Not a general web crawler.
- Not a substitute for worktree tools on *your* code.
- Personal API keys are device/human credentials, not a full org RBAC product in v1.

---

## 3. Product definition (short)

**Statement:** SPUR Context Service indexes third-party packages into a
versioned code graph and serves MCP tools that let agents search, read, and
traverse real dependency source — with on-demand indexing when a revision is
cold.

**Primary users**

1. Agents (SPUR brain/workers and other MCP clients) calling `external_*`
2. Developers configuring auth with `spur context …` and running `spur context mcp`
3. Organization integrations via Cognito client credentials (M2M)
4. Internal AWS workloads via SigV4

**Identity of an indexed unit:** `(source, package, revision)`

| Source type | Example `source` | Example `revision` |
|---|---|---|
| Registry (crates.io) | `registry:crates-io` | `1.0.197` |
| Git | `git:github.com/…` | commit SHA |

**External selectors** (carry only into `external_code_*` tools):

```text
pkg:serde@1.0.197::Deserialize
pkg:serde@1.0.197::Deserialize::deserialize
```

Do not mix with worktree selectors (`graph://symbol/<id>` → `code_*` only).

---

## 4. How to authenticate (CLI)

Two different jobs, two different credentials:

| Job | Commands | Credential | Used for |
|---|---|---|---|
| **Manage keys** | `spur context auth login` + `spur context key *` | Human OAuth (Cognito + PKCE) | Create / list / revoke keys |
| **Daily MCP** | `spur context mcp` | Personal API key | All routine `external_*` traffic |

Routine agent use should **not** refresh OAuth on every tool call. Login once,
create a key, run MCP with that key.

### 4.1 First-time workstation setup

```bash
# 1) Sign in (opens browser; callback http://127.0.0.1:8765/callback)
spur context auth login --profile workstation
# staging / custom deploy:
# spur context auth login --profile workstation --url https://<origin>

# 2) Create a personal key (scopes = what tools you may call)
spur context key create \
  --name workstation \
  --scope external.read \
  --profile workstation
# add more as needed:
#   --scope external.index
#   --scope external.status

# 3) Select the local key profile
spur context key list --profile workstation
spur context key use <PUBLIC_KEY_ID>

# 4) Start the external-context MCP server (stdio)
spur context mcp --profile <PUBLIC_KEY_ID>

# 5) Later: revoke or drop the OAuth management session
spur context key revoke <PUBLIC_KEY_ID> --profile workstation
spur context auth logout --profile workstation
```

### 4.2 Scopes

| Scope | Tools |
|---|---|
| `external.read` | `external_catalog`, `external_knowledge_context`, `external_code_search`, `external_code_read`, `external_code_callers`, `external_code_callees` |
| `external.index` | `external_index` |
| `external.status` | `external_index_status` |

`keys.manage` is OAuth-only for key lifecycle; it is **never** a legal API-key
scope. Multiple keys for one user share the same owner quota bucket — creating
more keys does **not** buy more rate limit.

### 4.3 Headless / CI credentials

Never pass the raw key as a CLI argument.

```bash
# Import once from stdin into a profile
spur context key add --stdin --profile automation

# Or inject per process (takes precedence over keyring / credentials file)
export SPUR_CONTEXT_SERVICE_API_KEY='…'
```

Optional restricted credentials file: `SPUR_CONTEXT_CREDENTIALS_FILE` (owner-only
permissions). Normal `.spur/config.toml` stores only non-secrets (URL, auth
mode, profile, public-id hint).

### 4.4 Other auth modes (operators / integrations)

| Mode | When |
|---|---|
| Personal API key | Human / local agent MCP (recommended day-to-day) |
| Cognito human OAuth | Key management; optional MCP via `POST /mcp/oauth` |
| Cognito M2M (`client_credentials`) | Long-lived org servers |
| AWS IAM SigV4 | Internal AWS callers on the default API route |

Discovery (public, no secrets): `GET /.well-known/spur-context-service`.

---

## 5. How to use the `external_*` tools

### 5.1 Choose the right plane first

```text
Question about THIS worktree?     → knowledge_context_pack_2 / code_*
Question about a dependency?      → external_*  (below)
Revision not indexed yet?         → external_index → external_index_status → retry
```

### 5.2 Tool map

| Tool | Use when |
|---|---|
| `external_knowledge_context` | Natural-language orientation in a package (“how does Deserialize work?”) |
| `external_code_search` | You know a symbol name / pattern |
| `external_code_read` | You have a selector and need source |
| `external_code_callers` | Impact inside the package (“who calls this?”) |
| `external_code_callees` | Behavior (“what does this call?”) |
| `external_catalog` | Browse packages, revisions, paths, file symbols |
| `external_index` | Make a missing revision indexable (or warm-complete if already present) |
| `external_index_status` | Poll a job returned by `external_index` |

### 5.3 Recommended multi-round flow (agents)

**1. Orient**

```json
{
  "package": "serde",
  "query": "how does Deserialize deserialize work",
  "revision": "1.0.197",
  "source": "registry:crates-io",
  "scope": "all",
  "limit": 8
}
```

→ tool: `external_knowledge_context`  
→ read evidence, confidence, and **`next`** selectors.

**2. Precision — carry the returned `selector`**

```json
{ "selector": "pkg:serde@1.0.197::Deserialize", "context_lines": 3 }
```

→ `external_code_read`

```json
{ "selector": "pkg:serde@1.0.197::Deserialize", "include_unresolved": true }
```

→ `external_code_callers` (impact: keep unresolved on)

```json
{ "selector": "pkg:serde@1.0.197::Deserialize", "include_unresolved": false }
```

→ `external_code_callees` (behavior: start with unresolved off; re-enable only if
the unresolved sample looks domain-relevant)

**3. Name known**

```json
{
  "package": "serde",
  "query": "Deserialize",
  "revision": "1.0.197",
  "symbol_kind": "trait",
  "limit": 20
}
```

→ `external_code_search` → then `external_code_read` on a hit.

**4. Browse**

```json
{
  "source": "registry:crates-io",
  "package": "serde",
  "revision": "1.0.197",
  "path": "src/",
  "limit": 50
}
```

→ `external_catalog`

**5. Cold revision (not indexed yet)**

```json
{
  "package": "my-crate",
  "revision": "0.1.0",
  "source_url": "https://github.com/org/repo/archive/refs/tags/v0.1.0.tar.gz",
  "source": "git:custom"
}
```

→ `external_index` returns one of:

| Status | Meaning |
|---|---|
| `complete` | Already indexed (warm path) — query immediately |
| `queued` + `job_id` | Build admitted — poll status |
| `rejected` | Abuse, rate limit, or queue full — backoff |

```json
{ "job_id": "<from external_index>" }
```

→ `external_index_status` until `complete` or `failed`, then retry search/read.

Notes:

- Admit is fast; builds are asynchronous. Do not block the agent on the worker.
- Concurrent identical requests collapse to one build.
- Status is **caller-scoped**: another caller’s `job_id` looks like `not_found`
  (non-enumerating).
- Pass **either** `revision` **or** `ref`, not both, on query tools.

### 5.4 Wire MCP into a client

| Client | Pattern |
|---|---|
| IDE / desktop agent | Register `spur context mcp --profile <id>` as an MCP stdio server |
| Scripts | `SPUR_CONTEXT_SERVICE_API_KEY` or a stored profile; call the same tools |
| Org servers | Cognito M2M token on the OAuth MCP route |
| Internal AWS | SigV4-signed invoke of the API |

Exact tool scopes are enforced on the body-selected tool name: a read-only key
cannot call `external_index`.

---

## 6. Anti-patterns

| Don’t | Do instead |
|---|---|
| Use worktree `code_*` to inspect dependency internals | `external_knowledge_context` / `external_code_*` |
| Feed `pkg:…` selectors into `code_read_symbol` | Keep selectors on the external plane |
| Block the session on a cold build | `external_index`, continue other work, poll status |
| Create many keys hoping for more quota | One owner bucket per human; scopes matter, not key count |
| Log, commit, or pass keys on the CLI | Keyring / stdin import / env |
| Assume private customer code is tenant-isolated in the shared catalog | Public packages only today |

---

## 7. Quick checklist

- [ ] `spur context auth login` succeeds (browser callback)
- [ ] Key created with the scopes you need
- [ ] `spur context mcp` starts with the selected profile
- [ ] Warm query works (`external_code_search` on a known package)
- [ ] Cold path works once (index → status `complete` → search)
- [ ] After revoke, calls fail within the revocation window (≤ 30s)

---

## 8. Related docs

| Doc | Role |
|---|---|
| [ARCHITECTURE.md](./ARCHITECTURE.md) | Medallion, planes, AWS resource map |
| `infra/spur-context-service/README.md` | Terraform, operator runbooks, API-key enablement |
| `docs/superpowers/specs/2026-06-22-code-context-service-design.md` | Original product design |
| `docs/superpowers/specs/2026-06-24-context-service-on-demand-indexing-design.md` | On-demand index contract |
| `docs/superpowers/specs/2026-07-11-context-service-api-key-auth-design.md` | Personal API-key auth |
| `docs/superpowers/evals/2026-06-28-external-mcp-tools-multi-round-eval.md` | Multi-round tool quality bar |
