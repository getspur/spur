#!/usr/bin/env python3
"""Extension coexistence probe for the DuckLake (1.5.2) vs DuckPGQ (1.4.x) conflict.

Run under each engine version (duckdb==1.5.2 and duckdb==1.4.5). For each engine,
probe whether ducklake and duckpgq can both INSTALL+LOAD, and whether core SQL/PGQ
(GRAPH_TABLE / ANY SHORTEST) works without the extension. The decisive output is
the install/load matrix: DuckDB extensions are ABI-locked to an exact engine
version, so a failed INSTALL means "no published binary for this (ext, engine,
platform) tuple" on extensions.duckdb.org.

Usage: python3 ext_matrix.py
"""
import sys
import traceback
import duckdb


def trial(con, label, fn):
    try:
        fn()
        return (label, "OK")
    except Exception as exc:  # noqa: BLE001 - probe wants broad capture
        msg = str(exc).strip().replace("\n", " ")[:240]
        return (label, f"FAIL [{type(exc).__name__}: {msg}]")


def probe():
    results = []
    base = duckdb.connect()

    # --- extension availability (the core of the conflict) ---
    results.append(trial(base, "install_ducklake", lambda: base.execute("INSTALL ducklake")))
    results.append(trial(base, "load_ducklake", lambda: base.execute("LOAD ducklake")))
    results.append(trial(base, "install_duckpgq", lambda: base.execute("INSTALL duckpgq")))
    results.append(trial(base, "load_duckpgq", lambda: base.execute("LOAD duckpgq")))

    # --- DuckLake local catalog round-trip (SQLite catalog + local data path) ---
    def ducklake_roundtrip():
        c = duckdb.connect()
        c.execute("INSTALL ducklake; LOAD ducklake;")
        c.execute("ATTACH 'ducklake:dl_probe_cat' AS dl (DATA_PATH '/tmp/dl_probe_data/');")
        c.execute("USE dl; CREATE TABLE IF NOT EXISTS probe_t AS SELECT 42 AS answer;")
        got = c.execute("SELECT answer FROM probe_t").fetchone()[0]
        assert got == 42, f"round-trip mismatch: {got}"
    results.append(trial(base, "ducklake_local_roundtrip", ducklake_roundtrip))

    # --- core SQL/PGQ WITHOUT any extension loaded (fresh connection) ---
    def core_pgq_direct():
        c = duckdb.connect()  # no extension loaded
        c.execute("CREATE TABLE v(id INT PRIMARY KEY, n VARCHAR);")
        c.execute("INSERT INTO v VALUES (1,'a'),(2,'b');")
        c.execute("CREATE TABLE e(src INT, dst INT, rel VARCHAR);")
        c.execute("INSERT INTO e VALUES (1,2,'calls');")
        c.execute(
            "CREATE PROPERTY GRAPH g AS "
            "VERTEX TABLES (v KEY(id)) "
            "EDGE TABLES (e SOURCE KEY(src) DESTINATION v KEY(dst));"
        )
        rows = c.execute(
            "SELECT * FROM GRAPH_TABLE(g "
            "MATCH (a)-[r:e]->(b) "
            "COLUMNS (a.id AS s, b.id AS t, r.rel AS rel))"
        ).fetchall()
        assert rows == [(1, 2, "calls")], f"direct match mismatch: {rows}"
    results.append(trial(base, "core_pgq_direct_match", core_pgq_direct))

    # --- ANY SHORTEST (GQL path syntax spur-analyst depends on) without extension ---
    def core_pgq_shortest():
        c = duckdb.connect()
        c.execute("CREATE TABLE v(id INT PRIMARY KEY, n VARCHAR);")
        c.execute("INSERT INTO v VALUES (1,'a'),(2,'b'),(3,'c');")
        c.execute("CREATE TABLE e(src INT, dst INT, rel VARCHAR);")
        c.execute("INSERT INTO e VALUES (1,2,'x'),(2,3,'y');")
        c.execute(
            "CREATE PROPERTY GRAPH g AS "
            "VERTEX TABLES (v KEY(id)) "
            "EDGE TABLES (e SOURCE KEY(src) DESTINATION v KEY(dst));"
        )
        # This is the exact pattern spur-analyst uses (lib.rs:719):
        #   MATCH p = ANY SHORTEST (a)-[r:e]->{1,N}(b)
        c.execute(
            "SELECT * FROM GRAPH_TABLE(g "
            "MATCH p = ANY SHORTEST (a:v)-[r:e]->{1,3}(b:v) "
            "COLUMNS (a.id AS s, b.id AS t, path_length(p) AS hops))"
        )
    results.append(trial(base, "core_pgq_any_shortest", core_pgq_shortest))

    # --- ANY SHORTEST WITH duckpgq extension loaded (if it installed) ---
    def ext_pgq_shortest():
        c = duckdb.connect()
        c.execute("LOAD duckpgq;")
        c.execute("CREATE TABLE v(id INT PRIMARY KEY, n VARCHAR);")
        c.execute("INSERT INTO v VALUES (1,'a'),(2,'b');")
        c.execute("CREATE TABLE e(src INT, dst INT, rel VARCHAR);")
        c.execute("INSERT INTO e VALUES (1,2,'x');")
        c.execute(
            "CREATE PROPERTY GRAPH g AS "
            "VERTEX TABLES (v KEY(id)) "
            "EDGE TABLES (e SOURCE KEY(src) DESTINATION v KEY(dst));"
        )
        c.execute(
            "SELECT * FROM GRAPH_TABLE(g "
            "MATCH p = ANY SHORTEST (a:v)-[r:e]->{1,3}(b:v) "
            "COLUMNS (a.id AS s, b.id AS t, path_length(p) AS hops))"
        )
    results.append(trial(base, "ext_pgq_any_shortest", ext_pgq_shortest))

    print(f"ENGINE: duckdb python={duckdb.__version__}  py={sys.version.split()[0]}")
    width = max(len(l) for l, _ in results)
    for label, status in results:
        print(f"  {label:<{width}}  {status}")


if __name__ == "__main__":
    try:
        probe()
    except Exception:
        traceback.print_exc()
        sys.exit(1)
