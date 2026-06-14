# RSS Named Source Handles Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Open Design notebook `/Users/kevintruong/.spur/scratch/Untitled133.ipynb`, artifact cell `49a52faa-c336-43e7-91be-1f068c53e6b5`
**Design epic:** Open Design pass approved in chat

**Goal:** Make RSS subscriptions feel like named notebook sources, so users can work with explicit handles such as `github_issues_openai_codex_entries` instead of only generic `rss_entries(url)`.

**Architecture:** Keep `crates/spur-notebook/rest-table-gateway/src/adapters/rss.rs` as the backend truth for now: it exposes `rss_routes`, `rss_feed(url)`, and `rss_entries(url)`. In the notebook wizard, derive a friendly source handle from the selected route or direct URL, generate a DuckDB view creation statement over `rss_entries(url)`, and make the friendly view query the primary handoff. This gives users an executable `select * from <handle>_entries` path without requiring dynamic backend table-function registration.

**Tech Stack:** React, TypeScript, Vitest, `@testing-library/react`, existing notebook REST datasource wizard, DuckDB SQL view syntax.

---

## Dependency DAG

```text
rss-source-handle-model
  -> rss-friendly-query-ux
      -> rss-named-source-verification
```

## File Structure Mapping

- `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx`
  - Add a small handle-generation helper for RSS direct URLs and RSSHub route selections.
  - Add source-handle UI, friendly table/view names, and create-view/query/raw-function handoff.
  - Keep backend implementation mapping explicit.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx`
  - Lock direct URL, RSSHub route, route parameter edit, and custom handle behaviors.
  - Assert the friendly query remains executable through a generated `create or replace view` statement.
- `crates/spur-notebook/rest-table-gateway/src/adapters/rss.rs`
  - Read-only context for this plan unless a worker proves a backend change is unavoidable.

---

### Task 1: RSS Source Handle Model

**Task ID:** `rss-source-handle-model`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx`
- Context only: `crates/spur-notebook/rest-table-gateway/src/adapters/rss.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] RSSHub route selections derive stable handles such as `github_issues_spur_dev_spur` and `github_issues_openai_codex`.
- [ ] Direct feed URLs derive stable handles such as `direct_example_feed_xml`.
- [ ] Handle generation lowercases text, replaces non-alphanumeric runs with `_`, trims leading/trailing `_`, and falls back to `rss_source` when the input has no usable characters.
- [ ] Existing `rss_entries(url)` and `rss_feed(url)` query templates still render.
- [ ] Focused wizard tests pass through `scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: RSS helper functions, `RssAttachStep` local state, existing RSS wizard tests.
- OUT of scope: backend dynamic table-function registration, persistent subscription database tables, unrelated datasource families.
- If deriving handles requires touching a shared datasource model outside `AddRestApiWizard.tsx`, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add failing tests**

Add assertions to the RSSHub route test:

```tsx
expect(screen.getByLabelText("Source handle")).toHaveValue(
  "github_issues_spur_dev_spur",
);
fireEvent.change(screen.getByLabelText("Parameter: owner"), {
  target: { value: "openai" },
});
fireEvent.change(screen.getByLabelText("Parameter: repo"), {
  target: { value: "codex" },
});
expect(screen.getByLabelText("Source handle")).toHaveValue(
  "github_issues_openai_codex",
);
```

Add assertions to the direct URL test:

```tsx
expect(screen.getByLabelText("Source handle")).toHaveValue(
  "direct_example_feed_xml",
);
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx
```

Expected: FAIL because `Source handle` does not exist.

- [ ] **Step 3: Add minimal handle helpers and state**

Implement helpers inside `AddRestApiWizard.tsx` near existing RSS helpers:

```ts
function rssSourceHandleFromParts(parts: string[]) {
  const value = parts
    .join("_")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return value || "rss_source";
}
```

Use route id plus parameter values for RSSHub mode, and `direct` plus URL host/path parts for direct mode. Store the handle in state only when the user overrides it; otherwise derive it from the current source.

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx
git commit -m "feat(spur-notebook): RSS1 add RSS source handles"
```

---

### Task 2: Friendly Query UX

**Task ID:** `rss-friendly-query-ux`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx`
- Context only: `crates/spur-notebook/rest-table-gateway/src/adapters/rss.rs`

**Depends on:** `rss-source-handle-model`

**Acceptance Criteria:**
- [ ] Source registration displays a friendly entries name `<handle>_entries`.
- [ ] Query handoff makes the friendly workflow primary:

```sql
create or replace view github_issues_openai_codex_entries as
select * from rss_entries('rsshub://github/issue/openai/codex');

select * from github_issues_openai_codex_entries
order by published_at desc;
```

- [ ] Raw `rss_entries(url)` remains visible as the implementation mapping.
- [ ] The copy clearly says the friendly table is a generated DuckDB view over the RSS table function.
- [ ] Direct URL and RSSHub flows both show friendly view SQL.
- [ ] Focused wizard tests pass through `scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: RSS source registration panel, query handoff panel, available table-functions copy, tests.
- OUT of scope: executing SQL automatically, changing notebook cell insertion APIs, backend adapter catalog changes.
- If implementation requires a new notebook command bridge, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add failing tests**

Add RSSHub assertions:

```tsx
expect(
  screen.getByText(
    "create or replace view github_issues_openai_codex_entries as",
  ),
).toBeInTheDocument();
expect(
  screen.getByText("select * from github_issues_openai_codex_entries"),
).toBeInTheDocument();
expect(
  screen.getByText(
    "select * from rss_entries('rsshub://github/issue/openai/codex');",
  ),
).toBeInTheDocument();
```

Add direct URL assertions:

```tsx
expect(
  screen.getByText("create or replace view direct_example_feed_xml_entries as"),
).toBeInTheDocument();
expect(
  screen.getByText("select * from direct_example_feed_xml_entries"),
).toBeInTheDocument();
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx
```

Expected: FAIL because the friendly view SQL is not rendered.

- [ ] **Step 3: Add friendly query templates**

In `RssAttachStep`, derive:

```ts
const entriesViewName = `${sourceHandle}_entries`;
const createEntriesViewSql = [
  `create or replace view ${entriesViewName} as`,
  `select * from rss_entries(${queryUrlLiteral});`,
].join("\\n");
const friendlyEntriesQuery = [
  `select * from ${entriesViewName}`,
  "order by published_at desc;",
].join("\\n");
```

Render three labeled blocks in this order:

1. Friendly entries query.
2. Create view SQL.
3. Raw backend mapping.

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx
git commit -m "feat(spur-notebook): RSS2 show named RSS view queries"
```

---

### Task 3: RSS Named Source Verification

**Task ID:** `rss-named-source-verification`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx`
- Test: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx`
- Context only: `crates/spur-notebook/rest-table-gateway/src/adapters/rss.rs`

**Depends on:** `rss-friendly-query-ux`

**Acceptance Criteria:**
- [ ] The RSS flow still states that backend table-functions are `rss_routes`, `rss_feed(url)`, and `rss_entries(url)`.
- [ ] The UI distinguishes "friendly view name" from "raw backend mapping" so users are not misled into thinking the adapter exposes dynamic table functions.
- [ ] Custom source handle edits update the friendly entries view name while preserving the same RSSHub/direct source URL.
- [ ] `scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx` passes.
- [ ] `scripts/spur-pnpm run typecheck` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: accessibility labels, explanatory copy, final test coverage, frontend typecheck.
- OUT of scope: durable subscription persistence, route-catalog live fetching, generated screenshots, backend Rust changes unless the current UI cannot truthfully represent the contract.
- If typecheck exposes unrelated failures outside the touched RSS wizard surface, emit `blocked` with the exact failure.

**Implementation:**
- [ ] **Step 1: Add final behavior tests**

Add assertions that a custom handle updates the friendly view:

```tsx
fireEvent.change(screen.getByLabelText("Source handle"), {
  target: { value: "codex_issue_feed" },
});
expect(
  screen.getByText("create or replace view codex_issue_feed_entries as"),
).toBeInTheDocument();
expect(
  screen.getByText("select * from codex_issue_feed_entries"),
).toBeInTheDocument();
expect(
  screen.getByText(
    "select * from rss_entries('rsshub://github/issue/openai/codex');",
  ),
).toBeInTheDocument();
```

- [ ] **Step 2: Run focused test to verify failure or pass**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx
```

Expected before final polish: FAIL if the custom-handle behavior is missing, PASS if Task 2 already covered it.

- [ ] **Step 3: Add final UI polish**

Make sure the RSS panels contain these exact user-facing concepts:

```text
Friendly view name
Raw backend mapping
Generated DuckDB view over rss_entries(url)
```

Keep `rss_entries(url)` visible in the implementation mapping and available table-functions list.

- [ ] **Step 4: Run verification**

Run:

```bash
scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx
scripts/spur-pnpm run typecheck
```

Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx
git commit -m "test(spur-notebook): RSS3 verify named RSS source queries"
```

---

## Self-Review

**Spec coverage:** The Open Design artifact’s source registry, friendly handle, friendly entries query, raw mapping, and subscribed source inventory are covered by the three tasks. The plan deliberately implements a generated view/query handoff, not backend dynamic table functions, because the current adapter catalog is static.

**Placeholder scan:** The plan contains no `TBD`, `TODO`, or open-ended "add tests" instructions. Each task has concrete tests, SQL strings, and verification commands.

**Type consistency:** The plan uses existing frontend files and RSS helper placement. The SQL names are plain DuckDB identifiers generated from sanitized lowercase handles.

**DAG validation:** `rss-source-handle-model -> rss-friendly-query-ux -> rss-named-source-verification` is acyclic. The chain is intentional because the friendly query UX depends on handle generation, and verification depends on both behaviors.

**beads compatibility:** Each task has a unique task ID, explicit dependency list, focused files, acceptance criteria, suggested worker, and scope boundary.
