# Spur Notebook Main Re-Alignment

Generated while merging local `main` into `spur/plan-integration/c651f9ec`.

- Local `main` head reviewed: `e53d26bc3bbb6ad7f2de05515eb56d40533de77f`
- Split comparison base: `0eafe797654e7b7e95a04fbb103d07bb3192e8cd`
- Committed notebook delta reviewed: 58 files, 4,697 insertions, 484 deletions
- Dirty notebook delta reviewed: 2 tracked files, 58 insertions, 7 deletions
- Untracked `crates/spur-notebook/jute-notebook/pnpm-lock.yaml` matched the standalone repo lockfile byte-for-byte

The in-tree `crates/spur-notebook` path remains deleted in the SPUR integration
branch. The reviewed notebook changes were applied to the standalone
`getspur/spur-notebook` checkout at `/private/tmp/spur-notebook` instead.

Committed local `main` notebook changes covered:

- SQL result table rendering
- API provider credential profile pool and saved-profile kernel env loading
- datasource schema catalog/table qualification work
- sidebar chat stream flushing, markdown rendering, and session-mode controls
- API datasource relation normalization and gateway relation exposure
- RSSHub subscription and route relation registration

Dirty tracked notebook changes layered on top:

- `jute-notebook/src-tauri/src/ports.rs`
- `src/mcp/tools/code_semantic_search.rs`
