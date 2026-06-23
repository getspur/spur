# DuckDB 1.4.5 LTS vs 1.5.2 — DuckLake / DuckPGQ Coexistence POC

**Date:** 2026-06-23
**Run on:** AWS Malaysia builder (`aws-my`), `m8gd.4xlarge` Graviton4, **arm64**, Debian 12.
Instance `i-040426b02b35934a6`. `linux_arm64` extension binaries (matches the
Graviton Lambda target for `spur-context-service`).

## Hypothesis under test

> DuckLake 1.0 only supports DuckDB **1.5.2**; DuckPGQ only supports DuckDB
> **1.4.x**; therefore the two cannot coexist on one engine and we must downgrade
> `spur-context-service` / `spur-analyst` to 1.4.5 LTS to get DuckPGQ.

## Method

`docs/spikes/duckdb-145-ducklake-duckpgq-probe/ext_matrix.py` runs under Python
`duckdb==1.5.2` and `duckdb==1.4.5` wheels (each bundles the exact libduckdb, so
`INSTALL <ext>` fetches the real per-(extension × engine × platform) binary from
`extensions.duckdb.org`). Then `INSTALL duckpgq` was probed across 1.1.1 → 1.5.2,
and the standalone `duckdb` CLI (v1.5.3) was checked for core SQL/PGQ support.

## Results

Two distribution channels were tested: the **default/core repo** (`INSTALL duckpgq`)
and the **community repo** (`INSTALL duckpgq FROM community` — `community` is an
unquoted keyword, canonical command per
https://duckdb.org/community_extensions/extensions/duckpgq.html).

### Extension matrix (Python bundled engine, linux_arm64)

| engine | ducklake (core) | duckpgq (community) | onager (community) |
|--------|:---:|:---:|:---:|
| **1.5.2** | ✅ OK | ❌ 404 | ❌ 404 |
| **1.4.5** | ✅ OK | ❌ 404 | ❌ 404 |
| **1.4.4** | ✅ OK | ✅ **OK** | ✅ **OK** |
| **1.4.3** | ✅ OK | ✅ **OK** | ✅ **OK** |
| **1.3.2** | ✅ OK | ✅ OK | (not probed) |
| **1.2.2** | ✅ OK | ✅ OK | — |
| **1.1.3** | ✅ OK | ✅ OK | — |

Install commands: `INSTALL ducklake;` (core repo),
`INSTALL duckpgq FROM community;` and `INSTALL onager FROM community;` (community
repo — `community` is an **unquoted keyword**, per
https://duckdb.org/community_extensions/extensions/{duckpgq,onager}.html).

### Extension versions on 1.4.3 (`duckdb_extensions()` + `onager_version()`)

| extension | version |
|-----------|---------|
| ducklake  | `de813ff` (git sha) |
| duckpgq   | `ffeee44` (git sha, cwida/duckpgq-extension) |
| onager    | `0.1.0-alpha.3` (semver via `onager_version()`); `0f2326a` (git sha in `duckdb_extensions()`) |

### Functional coexistence on 1.4.3 (proven) and 1.3.2 (proven)

All three load together on 1.4.3 (and 1.3.2):

- DuckLake local round-trip (SQLite catalog + `ATTACH ... (DATA_PATH ...)`): ✅ OK
- DuckPGQ `CREATE PROPERTY GRAPH gr VERTEX TABLES (v) EDGE TABLES (...)`
  (**no `AS`** keyword) + `GRAPH_TABLE (... MATCH (a:v)-[r:e]->(b:v) ...)`: ✅ OK
- DuckPGQ `MATCH p = ANY SHORTEST (a:v)-[r:e]->{1,3}(b:v)` (the spur-analyst
  pattern at `lib.rs:719`): ✅ OK
- Onager PageRank: ✅ OK — call with a **subquery relation**, not a string:
  `SELECT * FROM onager_ctr_pagerank((SELECT * FROM edges))` → 4 rows. SCC via
  `onager_cmm_components((SELECT * FROM edges))`. Note the **prefixed** names
  (`onager_ctr_*`, `onager_cmm_*`, `onager_pth_*`) — NOT the `onager_pagerank`
  shorthand in the spur-analyst SKILL.md; probe with
  `SELECT function_name FROM duckdb_functions() WHERE function_name LIKE 'onager%'`.

### Standalone CLI (`/usr/local/bin/duckdb` v1.5.3 Variegata)

- `SELECT ... FROM duckdb_extensions() WHERE extension_name='duckpgq'` → **not listed**.
- `CREATE PROPERTY GRAPH ...` → **`Parser Error: syntax error at or near "PROPERTY"`**.

## Conclusions

1. **The DuckLake ↔ 1.5.2 premise is FALSE.** DuckLake installs and loads cleanly
   on **all** tested versions (1.1.3 → 1.5.2). DuckLake is not a version blocker.

2. **DuckPGQ and Onager are community-repo extensions, available only for DuckDB
   ≤ 1.4.4.** `INSTALL duckpgq FROM community` / `INSTALL onager FROM community`
   (unquoted keyword) work through 1.4.4 and 404 on 1.4.5 and 1.5.2. Neither is in
   the core repo on any version.

3. **The single newest engine where all three coexist and function is 1.4.4.**
   1.4.3 is also fully proven (versions above). 1.4.5 buys nothing for DuckPGQ /
   Onager. Downgrading past 1.4.4 is unnecessary.

## Recommendation

- **`spur-context-service`: stay on `duckdb = "1.10502.0"` (engine 1.5.2).** It
  only needs DuckLake, which works on 1.5.2. Do not downgrade.
- **`spur-analyst`: if in-process PGQ + Onager are required, the engine must be
  ≤ 1.4.4** (newest = 1.4.4; 1.4.3 also proven) **and** setup must run
  `INSTALL duckpgq FROM community; INSTALL onager FROM community;` before LOAD.
  spur-analyst currently only does `LOAD duckpgq;` (lib.rs:658,712) with no
  install, so it silently fails on 1.5.2 today. Options:
  1. Pin spur-analyst to **1.4.4** + add the two community `INSTALL`s (full PGQ +
     Onager, newest usable engine; ~3 months behind 1.5.2 and a per-process
     network fetch on first install).
  2. Stay on 1.5.2 and rely on the existing recursive-SQL fallback (lib.rs:499
     guard) as the primary path — no PGQ/Onager, but no downgrade either.
  3. Split pins: `spur-context-service` on 1.5.2 (DuckLake only), `spur-analyst`
     on 1.4.4 (DuckLake + duckpgq + onager). Workspace allows per-crate pins; cost
     is a second bundled libduckdb compile.
- Community-install production caveat: `INSTALL ... FROM community` hits
  `community-extensions.duckdb.org` at runtime; in a Lambda cold start that is a
  network fetch keyed to the exact (engine × platform) tuple.

## DuckLake remote-S3 catalog — RESOLVED on 1.4.4 (proven)

`spur-context-service`'s remote-S3-catalog path was verified end-to-end on a real
S3 bucket (`ap-southeast-5`, arm64 Graviton builder), engine 1.4.4 + ducklake
`de813ff`. Pattern per
https://ducklake.select/docs/stable/duckdb/guides/public_ducklake_on_object_storage:

**Writer** (indexer): local DuckDB catalog file + S3 `DATA_PATH`:
```sql
ATTACH 'ducklake:/tmp/writer.ducklake' AS dl (DATA_PATH 's3://bucket/ro/data/');
-- create partitioned spur_context tables, insert, then:
CHECKPOINT;  -- flush WAL so the catalog file is self-contained
-- upload /tmp/writer.ducklake to s3://bucket/ro/catalog.ducklake
```

**Reader** (query Lambda): attach the S3 catalog read-only — `DATA_PATH` is stored
in the catalog metadata, so it is NOT re-specified:
```sql
ATTACH 'ducklake:s3://bucket/ro/catalog.ducklake' AS dl;   -- read-only, works
ATTACH 's3://bucket/ro/catalog.ducklake' AS dl (TYPE ducklake);  -- catalog.rs:270 form, works
```

All four reader scenarios passed: count, partition-predicate read, the
`catalog.rs:270` `(TYPE ducklake)` syntax, and `snapshots()` time-travel over the
S3 catalog (4 snapshots).

> **Correction to the first S3 run:** an earlier trial that did
> `ATTACH 'ducklake:s3://.../catalog.ducklake'` *before the file existed* failed
> with `Cannot open database "..." in read-only mode: database does not exist`.
> That is the **expected** error when you try to *create* a catalog on read-only
> S3 — the docs quote it verbatim. The catalog must be created locally by a
> writer, checkpointed, and uploaded; only then does the read-only S3 attach
> work. Conclusion: **read-only S3 DuckDB catalog works on 1.4.4**; the
> catalog.rs remote branch is sound for the query path, and the indexer needs the
> local-catalog-then-upload flow.

## Open follow-up

- None for the version question. Remaining work (if pursuing 1.4.4) is
  integration: add `INSTALL duckpgq FROM community; INSTALL onager FROM community;`
  to spur-analyst setup, implement the writer upload step for spur-context-service,
  and validate the split-pin (spur-context-service on 1.5.2 / spur-analyst on 1.4.4)
  actually compiles in the workspace.

## Artifacts

- `ext_matrix.py` — the probe (run under each engine wheel).
- Raw output captured in the POC session (this VM self-terminates after 30 min idle).
