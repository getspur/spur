REQUEST-CHANGES — the core path is sound, but valid/accepted YAML `tools` forms can be corrupted or remain unaugmented, recreating the silent tool-hiding failure.

## Findings

Critical: no findings.

High: no findings.

Medium:

1. [render.rs:104](/Volumes/Projects/spur/.spur/worktrees/54a16533-28a9-41de-8207-8add9e44c433/crates/spur-core/src/agent_profiles/render.rs:104) — The rewrite is not YAML-aware.

   [AgentProfile::parse](/Volumes/Projects/spur/.spur/worktrees/54a16533-28a9-41de-8207-8add9e44c433/crates/spur-core/src/agent_profiles/mod.rs:42) accepts a multiline form such as:

   ```yaml
   tools:
     - Read
     - Edit
   ```

   as `tools = Some("")`. Augmentation then rewrites only the header, leaving the sequence entries beneath a scalar and producing malformed YAML. Similarly, `tools: Read, Edit # comment` appends MCP names after `#`, so they remain commented out. Quoted/flow-list forms are also corrupted, while `tools : Read` is missed entirely.

   Concrete failure: a valid or previously usable profile either fails to load or still hides all worker-MCP tools.

   Suggested fix: parse `tools` structurally and serialize a canonical supported form. At minimum, explicitly support block lists, comments, quoted/flow values, and key whitespace—or reject unsupported forms during profile loading rather than silently rewriting them.

Low:

2. [worker_attempt.rs:988](/Volumes/Projects/spur/.spur/worktrees/54a16533-28a9-41de-8207-8add9e44c433/crates/spur-core/src/orchestrator/delegation/worker_attempt.rs:988) — The positive integration test bypasses the actual transport/enablement gate.

   [The test](/Volumes/Projects/spur/.spur/worktrees/54a16533-28a9-41de-8207-8add9e44c433/crates/spur-core/src/orchestrator/delegation/worker_attempt.rs:4005) calls `materialize_profile(..., Some(&tools))` directly. It would still pass if the `worker_mcp_servers.is_empty()` or `TransportKind::Acp` conditions regressed.

   Concrete failure: `enable_worker_mcp=false` or a stream-json worker could receive allowlisted names for unavailable tools without any test failing.

   Suggested fix: extract a small gate helper and table-test kind × transport × server-presence, or exercise the gate through `run_one_worker_attempt`.

3. [worker_attempt.rs:988](/Volumes/Projects/spur/.spur/worktrees/54a16533-28a9-41de-8207-8add9e44c433/crates/spur-core/src/orchestrator/delegation/worker_attempt.rs:988) — Tool names are allocated for every profile-bearing ACP worker.

   For Codex, Kiro, OpenCode, or Generic ACP workers, `worker_mcp_claude_tool_names()` builds the registry and allocates the complete name vector before `materialize_profile` discards it based on kind.

   Suggested fix: include the Claude-kind check in the `.then(...)` condition, or compute the vector inside the Claude branch. This is minor and not a correctness blocker.

## Goals 1–6

1. String handling

   - Exact `---` fence detection is consistent with `AgentProfile::parse`; a body-only `tools:` line is not rewritten.
   - CRLF is normalized to LF before rewriting. The marker hashes that rewritten LF content, so this causes a deterministic one-time managed-file rewrite, not marker inconsistency.
   - Empty comma-separated entries are filtered. Internal registry-derived extras cannot be empty.
   - `tools:Read` is recognized and canonicalized.
   - Block lists, comments, quoting/flow lists, and some YAML whitespace variants are unsafe: finding 1.
   - The `!replaced` fallback safely returns the original profile. For a normally parsed inline `tools:` field it should be unreachable; it remains a fail-closed fallback for inconsistent manually constructed profiles.

2. Marker and ownership interactions

   Correct as written:

   - Previously managed, non-augmented file → its embedded hash still validates, but it differs from the new expected render → `ManagedDifferent` → overwritten.
   - Matching augmented file → `Unchanged`.
   - User-edited managed file → disk hash fails against its embedded marker first → `Edited` → retained.
   - Unmarked user file → `NoMarker` → retained.

   Augmentation does not create a path that converts an ordinary user edit into an overwrite. Only a user who also recomputes the ownership marker can make content appear managed, which is existing marker semantics.

3. Gating

   The standard combinations are correct:

   - Nonempty worker-MCP servers + ACP + Claude kind → augmented.
   - `enable_worker_mcp=false` → empty vector → not augmented.
   - Claude over StreamJson/Stdio/CliWrap → not augmented because those adapters cannot deliver the server.
   - Non-Claude ACP kinds → not augmented inside `materialize_profile`.

   `ClaudeStreamJson` is not strictly unreachable through the ACP gate: `AgentKind` and `TransportKind` are explicitly orthogonal, and validation does not forbid `ClaudeStreamJson + Acp`. In that unusual configuration the real connection is native ACP and does receive MCP servers, so augmentation is still behaviorally correct.

4. Name derivation

   Correct as written.

   - `mcp__spur-worker-mcp__<tool>` matches the expected Claude naming contract.
   - The shared server-name constant is used by dispatch, server metadata, and allowlist derivation.
   - `worker_tools_list()` uses `list_tools()`, which filters denied tools at [registry.rs:189](/Volumes/Projects/spur/.spur/worktrees/54a16533-28a9-41de-8207-8add9e44c433/crates/spur-mcp/src/registry.rs:189).
   - No restricted brain-only tool is reintroduced. Persona allowlisting does not bypass server-side authorization; reviewer-only tools remain server-authorized.

5. Test quality

   The tests do pin:

   - preservation of `Read, Edit`;
   - deduplication when an injected tool already exists;
   - byte-identical passthrough for no-tools profiles;
   - complete, unique derivation from the advertised registry.

   Missing coverage: YAML variants, CRLF, body-only `tools:`, ownership migration, and the actual transport/enablement truth table. The positive orchestration test does not pin the gate itself.

6. General assessment

   - Naming and organization fit the surrounding code.
   - `render_for_kind` is unchanged; graph inspection found its production consumers in `materialize_profile` and the spur-tui mentions registry. The TUI caller only checks renderability and is unaffected.
   - No security vulnerability or significant performance regression found. The unnecessary non-Claude allocation is minor.
   - `augment_tools_allowlist` and `WORKER_MCP_SERVER_NAME` could be `pub(crate)` rather than public.
   - The commit split matches the repository’s TDD convention: `adc54d3e0` is compile-ready but intentionally red through stubs, followed by the implementation commit. The additional wiring test landed with the feature commit rather than in the red commit, but that is not itself a broken-history finding.
   - Per instruction, I did not run builds, tests, or coverage. Read-only verification found a clean worktree, no relevant code changes after `3af2a5836`, and no `git diff --check` errors. Coverage above 80% was not independently confirmed.

## Follow-up hardening

- Add a table-driven frontmatter suite covering CRLF, `tools:Read`, comments, quoted/flow values, block lists, empty entries, and body-only `tools:`.
- Add an ownership transition test: old managed → overwritten, augmented managed → unchanged, edited managed → retained.
- Add a pure gating truth table for kind, transport, and worker-MCP server presence.