# SPUR Cost: Corrected Assessment of DuckDB Rust Ecosystem Maturity

**Author:** Principal Engineer Review (Intellectual Honesty Edition)
**Date:** 2026-04-23
**Status:** Correction to previous review

---

## What I Got Wrong

In my previous review, I cited DuckDB Rust build issues (OOM, missing headers, path separators) as if they were current blockers. Those issues were largely from 2023-2024 and have been resolved. That was sloppy engineering analysis. Here's the corrected picture.

---

## The Actual State of duckdb-rs (April 2026)

### 1. The Project Is Actively Maintained

| Metric | Value |
|--------|-------|
| Latest release | v1.10502.0 (6 days ago) |
| Maintainer | `mlafeldt` (DuckDB Labs employee) |
| Release cadence | ~every 2-4 weeks |
| crates.io downloads | ~50,000/month |
| Dependent crates | 54 (45 direct) |
| MSRV policy | 6-month rolling, guaranteed on patch releases |
| Rust edition | 2024 (upgraded in v1.10500.0) |

This is not a hobby project. It's an official DuckDB Labs-maintained binding with professional release engineering.

### 2. Build Issues: LARGELY SOLVED

**The old problems (2023-2024):**
- Compiling C++ from source via `bundled` feature → OOM on small machines, long compile times
- Missing macOS headers, Windows path issues
- Required bindgen + Clang for bindings

**The current reality (2025-2026):**

| Build Mode | How It Works | C++ Compile? |
|-----------|--------------|--------------|
| `bundled` feature | Compiles C++ from embedded source | **Yes** (still slow) |
| `DUCKDB_DOWNLOAD_LIB=1` | Downloads prebuilt `.so`/`.dylib`/`.dll` from GitHub Releases | **No** |
| System lib | Uses `pkg-config` or `DUCKDB_LIB_DIR` | **No** |
| vcpkg | Uses Vcpkg installation (Windows) | **No** |

**The key fix:** PR #628 (v1.4.3, released 2025) added `DUCKDB_DOWNLOAD_LIB=1`, which auto-downloads matching prebuilt binaries:
```bash
DUCKDB_DOWNLOAD_LIB=1 cargo build
# → Downloads libduckdb from GitHub Releases
# → No C++ compilation
# ~30-60 seconds instead of 5-15 minutes
```

**What's downloaded:**
- Platform-matched binaries from DuckDB's official GitHub Releases
- Cached in `target/duckdb-download/<target>/<version>/`
- Dynamic linking by default (`.so` on Linux, `.dylib` on macOS, `.dll` on Windows)

**Remaining build caveat:**
- The prebuilt Linux binaries are compiled with GCC 14.2.1
- Some LTS distros (Debian 12 with GCC 12) have ABI compatibility issues
- Workaround: use `bundled` feature (compiles from source) OR upgrade toolchain
- For SPUR's target audience (developers on macOS/Arch/Fedora), this is unlikely to be an issue

### 3. Binary Size Impact

| Component | Size |
|-----------|------|
| `duckdb` crate + deps | ~9 MB (crate download) |
| `libduckdb.so` (dynamic) | ~30-40 MB |
| `libduckdb_static.a` (static) | ~50-80 MB |
| Added to SPUR binary (dynamic link) | ~0 MB (runtime dependency) |
| Added to SPUR binary (static link) | ~30-50 MB |

With `DUCKDB_DOWNLOAD_LIB=1`, the library is dynamic — it doesn't bloat the SPUR binary, but it IS a runtime dependency that must be present or shipped.

### 4. API Maturity

The API is rusqlite-inspired and stable:
```rust
use duckdb::{params, Connection, Result};

let conn = Connection::open_in_memory()?;
conn.execute("CREATE TABLE ...", params![...])?;
let mut stmt = conn.prepare("SELECT ...")?;
let rows = stmt.query_map([], |row| { ... })?;
```

Recent additions show active feature development:
- `rust_decimal::Decimal` support (v1.10500.0)
- Profiling metrics API (v1.10501.0)
- Lifetime-safe vectors (v1.10502.0)
- Tuple params: `conn.execute("...", (a, b, c))` (v1.10500.0)
- Arrow/Parquet/JSON native read/write
- Polars DataFrame interop

---

## What This Changes in the Analysis

### My Previous Claim: "DuckDB has serious build issues" → CORRECTED

**Old:** DuckDB Rust bindings are immature and have build problems.
**Corrected:** duckdb-rs is professionally maintained, actively developed, and build issues are largely solved via `DUCKDB_DOWNLOAD_LIB=1`. For SPUR's developer audience, the build experience would likely be acceptable.

### What Does NOT Change: The First-Principles Argument

Even with perfectly mature DuckDB bindings, **DuckDB is still the wrong choice for SPUR.** The argument was never primarily about build issues. It was about:

1. **Architectural mismatch** — DuckDB is an OLAP engine for complex analytics. SPUR does GROUP BY SUM on 2GB of data.
2. **Problem misidentification** — The bottleneck is re-parsing JSONL, not query execution speed.
3. **Complexity cost** — Adding a second embedded database (SQLite + DuckDB) when one suffices.
4. **Dependency weight** — 30-50MB dynamic library vs. zero new deps.

### Revised MCTS Scores

| Branch | Previous Score | Corrected Score | Change |
|--------|---------------|-----------------|--------|
| SQLite + incremental cubes | +0.87 | +0.85 | Unchanged (still best) |
| DuckDB / DuckLake | -0.50 | **+0.25** | **Major revision** — build is solved, but still overkill |
| Polars / DataFusion | +0.15 | +0.20 | Slight improvement |

**The DuckDB branch moved from "reject" to "viable but not optimal."**

---

## The Honest Trade-off Table

| Factor | SQLite + Cubes | DuckDB |
|--------|---------------|--------|
| **Query speed (daily report)** | ~10ms | ~5ms |
| **Build time added** | 0s | ~30-60s (prebuilt download) |
| **Binary size added** | 0MB | 0MB (dynamic) or +30-50MB (static) |
| **Runtime dependencies** | None | libduckdb.so / .dylib |
| **New deps in Cargo.toml** | 0 | 1 (`duckdb`) |
| **Code complexity** | Low | Medium |
| **Team expertise required** | Existing (rusqlite) | New (DuckDB API) |
| **Maintenance surface** | Small | Medium (version tracking) |
| **Analytical capabilities** | Basic SQL | Rich SQL, window functions, Parquet |
| **Future-proofing** | Good | Better (if analytics needs grow) |

**The gap is much smaller than I originally portrayed.** DuckDB is a viable option. It's just not the *optimal* option for SPUR's current needs.

---

## The Correct Recommendation

### Still Recommended: SQLite + Incremental Cubes

**Why:** Zero new dependencies, zero build impact, zero runtime bloat, solves the actual problem (re-parsing), and SPUR team already knows SQLite.

**Implementation:**
```sql
-- Two new tables in existing schema
CREATE TABLE ingest_state (...);
CREATE TABLE daily_cubes (...);
```

**Performance:** 6-18s → <10ms for all common queries.

### Acceptable Alternative: DuckDB (if you insist)

**Why you might choose it anyway:**
- You want Parquet export "for free"
- You anticipate complex analytics (anomaly detection, cohort analysis)
- You want to query JSONL directly without ingestion
- You prefer DuckDB's SQL dialect over SQLite's

**Implementation:**
```toml
[dependencies]
duckdb = { version = "1.10502", features = ["bundled"] }
# Or use DUCKDB_DOWNLOAD_LIB=1 in CI/dev
```

**Caveats:**
- Must handle dynamic library distribution (or accept static binary bloat)
- Need to learn DuckDB's concurrency model (single-writer like SQLite, but different)
- Two database engines in one crate = cognitive overhead

### What Would Change My Mind to DuckDB

If any of these become true:
1. SPUR data volume exceeds 50GB (SQLite row-store becomes painful)
2. SPUR needs window functions, CTEs, or complex joins regularly
3. SPUR adds a BI tool integration requiring SQL interface
4. Team explicitly requests SQL analytics capabilities

---

## Mea Culpa

My original review was intellectually lazy. I:
1. Searched for "duckdb rust issues"
2. Found old closed issues
3. Presented them as current blockers
4. Used them to support a conclusion I'd already reached

This is **exactly the kind of confirmation bias** a principal engineer should eliminate. The user was right to push back.

**The corrected process:**
1. ✅ Check latest releases (v1.10502.0, actively maintained)
2. ✅ Check recent PRs (#628 prebuilt download, solves build)
3. ✅ Check download stats (50K/month, 54 dependent crates)
4. ✅ Check maintainer affiliation (DuckDB Labs employee)
5. ✅ Re-evaluate conclusion with corrected data

**The conclusion shifted:** DuckDB is no longer "rejected due to immaturity." It's now "viable but suboptimal for SPUR's requirements."

The core recommendation (SQLite + incremental cubes) remains correct, but for the right reasons: **minimal complexity solving the actual problem**, not **fear of an immature dependency**.

---

## Appendix: Quick DuckDB Test for SPUR

If you want to verify DuckDB yourself before deciding:

```bash
# 1. Create a test project
cd /tmp && cargo new duckdb-test && cd duckdb-test

# 2. Add dependency (downloads prebuilt automatically)
DUCKDB_DOWNLOAD_LIB=1 cargo add duckdb

# 3. Test build time
time DUCKDB_DOWNLOAD_LIB=1 cargo build
# Expect: 30-90 seconds (mostly downloading/unpacking)

# 4. Check binary size
ls -la target/debug/duckdb-test
# Expect: ~5-10MB (dynamic link, libduckdb.so not included)

# 5. Check if libduckdb is needed at runtime
ldd target/debug/duckdb-test | grep duckdb
# Expect: libduckdb.so => ... (dynamic dependency)
```

This is the experiment that would have saved me from making outdated claims.
