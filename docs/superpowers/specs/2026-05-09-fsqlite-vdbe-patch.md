# fsqlite Workspace Patch — Pinned at Upstream Pre-Nightly Commit

Date: 2026-05-09

## Context

SPUR's `beads_rust` fork is pinned at `543495d5c52c9996acfea8c534708b6009bbe22e`
(`spur-pm/backport-270-drop-checkpoint`). Both the old (`08dabbb`) and current
fork revs depend on the same `fsqlite-* 0.1.2` crates from crates.io. The
0.1.2 release on crates.io has a correctness bug ("Bug A" below) that causes
SPUR to refuse to open `.beads/beads.db` files with non-zero
`reserved_per_page`. Reverting the fork pin does not fix this.

Upstream (https://github.com/Dicklesworthstone/frankensqlite) has fixed Bug A
on `main` but has not yet published a new crates.io release. Master has also
moved to nightly-Rust features (`#![feature(core_intrinsics)]`,
`#![feature(portable_simd)]`) which SPUR cannot consume on stable rustc.

This workspace patches every fsqlite-* crate that beads_rust transitively
depends on with a vendored snapshot of upstream commit
**`dd9b457a02f74d7b4f5d5db9d7d3c8e7e9d6c5b4`** (March 31, 2026). That commit
is the latest point on upstream's history where:

1. Bug A fix `6501e2aa` ("correct reserved-bytes page-size handling across
   B-tree operations") is already merged.
2. No `#![feature(...)]` directives have been added yet — the next commit
   (`9abedc20`) introduces `core_intrinsics` in `fsqlite-btree`.

## Vendor Layout

```
vendor/
├── Cargo.toml              # own workspace, excluded from spur's workspace
├── fsqlite/                ┐
├── fsqlite-ast/            │
├── fsqlite-btree/          │
├── fsqlite-core/           │
├── fsqlite-error/          │  20 crates: every fsqlite-* SPUR transitively
├── fsqlite-ext-fts5/       │  depends on. Upstream siblings not pulled in:
├── fsqlite-ext-icu/        │  fsqlite-cli, fsqlite-c-api, fsqlite-wasm,
├── fsqlite-ext-json/       │  fsqlite-e2e, fsqlite-harness, fsqlite-ext-fts3,
├── fsqlite-ext-misc/       │  fsqlite-ext-session.
├── fsqlite-ext-rtree/      │
├── fsqlite-func/           │
├── fsqlite-mvcc/           │
├── fsqlite-observability/  │
├── fsqlite-pager/          │
├── fsqlite-parser/         │
├── fsqlite-planner/        │
├── fsqlite-types/          │
├── fsqlite-vdbe/           │
├── fsqlite-vfs/            │
└── fsqlite-wal/            ┘
```

The vendored crates are byte-for-byte copies of the upstream tree at
`dd9b457a` — no local diff is applied. Bug A's fix is already present in those
files (`crates/fsqlite-vdbe/src/engine.rs:407` uses
`header.page_size.usable(header.reserved_per_page)`).

`vendor/Cargo.toml` is the upstream workspace `Cargo.toml` from `dd9b457a`,
trimmed to only the 20 crates above and with `path = "crates/fsqlite-X"`
flattened to `path = "fsqlite-X"`.

The workspace root `/Volumes/Projects/spur/Cargo.toml` adds
`exclude = ["vendor"]` to prevent the vendored crates from being treated as
SPUR workspace members and hooks each crate via `[patch.crates-io]` path
overrides.

## Bug A: Reserved-Byte Overflow Reads — Fixed Upstream

Manifestation:

```
PM service initialization failed: Database error:
  database is busy (snapshot conflict on pages: page 1701064047 > snapshot db_size 4)
```

Or in production-sized DBs:

```
snapshot conflict on pages: page 772409891 > snapshot db_size 2680
```

The `page N > snapshot db_size M` numbers are markdown text bytes from issue
bodies being misinterpreted as 32-bit big-endian page pointers. The bug is
that fsqlite-vdbe's b-tree cursor reads overflow-pointer offsets using full
`page_size` instead of `usable_size = page_size - reserved_per_page`. With
`reserved_per_page = 12` (as observed on the user's `.beads/beads.db`), every
overflow-pointer read lands 12 bytes too early and decodes payload bytes as a
page reference.

Upstream commit `6501e2aa` fixes this by:

1. Adding `reserved_per_page: u8` to `ConnectionPragmaState`.
2. Reading the value from the SQLite header in
   `bootstrap_pragma_state_from_storage`.
3. Threading it into `VdbeEngine::set_reserved_per_page`.
4. Computing `btree_usable_size()` as `page_size.usable(reserved_per_page)`.
5. Updating every `BtCursor::new_with_index_desc` site (engine.rs lines
   ~5498, ~10083, ~10166, ~10277) to pass the usable size, with the in-memory
   fallback continuing to use `page_size.get()`.

Regression coverage: `crates/spur-pm/tests/fsqlite_patched_deps.rs::fsqlite_reads_reserved_byte_overflow_payloads`
constructs a SQLite file with `reserved_per_page = 12` via rusqlite's
`SQLITE_FCNTL_RESERVE_BYTES` file control, writes a row with a payload that
overflows the page, and asserts the patched fsqlite reads it back without a
`BusySnapshot` error. Pre-fix, the test reproduces the error; post-fix, it
passes.

## Bug B: Page-Ownership Aliasing on Writes — Unresolved Upstream

Manifestation: spur writes ~10 issues to a clean `reserved=0` DB and the
b-tree corrupts:

```
Tree 1443 page 1738 cell 0: invalid page number 218103808   (0x0D000000)
Tree 2450 page 1684 cell 52: 2nd reference to page 2704
Tree 9 page 9 cell 0: 2nd reference to page 2551
Tree 2007 page 1782: btreeInitPage() returns error code 11
+ ~30 index entry inconsistencies
```

Diagnosis (from this session): page 1751 is referenced as an overflow page
from page 1738, but page 1751 is actually a table-leaf page. Its first byte
(`0x0D`, the leaf-page header type byte) decodes as the bogus overflow pointer
`0x0D000000` (= 218103808). This is page-ownership aliasing — the page
allocator is returning a still-in-use page as "free."

Upstream has an unmerged `fix/freelist-persist-c390` branch (March 2026, on
older 0.1.1 code) with commits like `da5c80f fix(pager): persist freelist to
SQLite freelist pages` and `c390150 fix(pager): repair page1 header after WAL
checkpoint growth`. That branch may address Bug B but is far behind master,
unmerged, and pre-Bug-A-fix.

Master's pager work since `dd9b457a` is performance-focused
(`41a950b/14f6727/c65caad` for buffer recycling) and does not address the
freelist correctness issue.

Regression coverage: `beads_large_issue_writes_leave_sqlite_integrity_ok`
writes 80 issues with overflow-sized bodies and asserts integrity. This passes
under `dd9b457a`, indicating Bug B requires a more specific trigger we have
not reproduced. Codex (a prior worker) deliberately did not ship a guess-fix.

Follow-up tracked separately. Suggested instrumentation when Bug B recurs:
log every `allocate_page` / `free_page` / freelist-trunk update; capture full
DB state on first integrity_check failure.

## Maintenance

- **Bumping the vendor pin**: when upstream merges Bug B's fix or otherwise
  publishes useful work, repoint by re-running the steps in
  `scripts/`-equivalent flow: clone upstream, `git checkout <new-rev>`, copy
  `crates/fsqlite-{20 names}` into `vendor/`, re-flatten the workspace
  `path = "crates/fsqlite-X"` → `path = "fsqlite-X"`. Verify with
  `scripts/spur-cargo build -p spur-cli` and the regression tests.

- **Cap on bumping**: do not bump past `9abedc20` (first
  `#![feature(core_intrinsics)]`) without either upstream gating the nightly
  features behind a cargo feature or SPUR migrating to nightly Rust.

- **Removal**: when crates.io publishes 0.1.3+ with Bug A's fix and beads_rust
  is bumped to it, delete `vendor/`, the `[patch.crates-io]` block in the
  workspace `Cargo.toml`, the `exclude = ["vendor"]` line, the `fsqlite =
  "0.1.2"` dev-dep in `crates/spur-pm/Cargo.toml`, and (optionally) the
  regression tests if duplicated upstream.

## Verification

```bash
$ scripts/spur-cargo build -p spur-cli
... Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 47s

$ scripts/spur-cargo test -p spur-pm --test fsqlite_patched_deps
... running 2 tests
... test fsqlite_reads_reserved_byte_overflow_payloads ... ok
... test beads_large_issue_writes_leave_sqlite_integrity_ok ... ok
... 2 passed; 0 failed
```
