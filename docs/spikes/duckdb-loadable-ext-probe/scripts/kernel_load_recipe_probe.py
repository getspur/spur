#!/usr/bin/env python3
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path


PROBE_DIR = Path(__file__).resolve().parents[1]
EXTENSION = PROBE_DIR / "build" / "release" / "spur_probe.duckdb_extension"


def run_case(name: str, code: str, extra_env: dict[str, str] | None = None) -> None:
    env = os.environ.copy()
    env["SPUR_PROBE_EXTENSION"] = str(EXTENSION)
    if extra_env:
        env.update(extra_env)

    result = subprocess.run(
        [sys.executable, "-c", code],
        cwd=PROBE_DIR,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    print(f"=== {name} ===")
    print(f"returncode {result.returncode}")
    stdout = result.stdout.rstrip()
    stderr = result.stderr.rstrip()
    if stdout:
        print(stdout)
    if stderr:
        print("--- stderr ---")
        print(stderr)


COMMON = r'''
import os
from pathlib import Path

import duckdb

ext = Path(os.environ["SPUR_PROBE_EXTENSION"])
ext_sql = str(ext).replace("'", "''")

def report(label, fn):
    try:
        value = fn()
    except Exception as err:
        print(f"{label} ERROR {type(err).__name__}: {err}")
    else:
        print(f"{label} OK {value}")

print("duckdb", duckdb.__version__)
print("extension", ext)
'''


def main() -> None:
    if not EXTENSION.exists():
        raise SystemExit(f"missing extension artifact: {EXTENSION}")

    run_case(
        "default-load-no-config",
        COMMON
        + r'''
report("duckdb.sql LOAD", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
''',
    )

    run_case(
        "default-connection-config-arg",
        COMMON
        + r'''
report(
    "duckdb.default_connection(config=...)",
    lambda: duckdb.default_connection(config={"allow_unsigned_extensions": "true"}),
)
''',
    )

    run_case(
        "set-after-default-connect",
        COMMON
        + r'''
con = duckdb.default_connection()
report("SET allow_unsigned_extensions", lambda: con.execute("SET allow_unsigned_extensions = true"))
report("duckdb.sql LOAD after SET", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
''',
    )

    run_case(
        "duckdb-env-prefix",
        COMMON
        + r'''
print("DUCKDB_ALLOW_UNSIGNED_EXTENSIONS", os.environ.get("DUCKDB_ALLOW_UNSIGNED_EXTENSIONS"))
report("duckdb.sql LOAD", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
''',
        {"DUCKDB_ALLOW_UNSIGNED_EXTENSIONS": "true"},
    )

    run_case(
        "connect-config-no-rebind",
        COMMON
        + r'''
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
report("con.execute LOAD", lambda: (con.execute(f"LOAD '{ext_sql}'"), "loaded")[1])
report("con.sql SELECT", lambda: con.sql("SELECT * FROM spur_probe()").fetchall())
report("module duckdb.sql SELECT", lambda: duckdb.sql("SELECT * FROM spur_probe()").fetchall())
''',
    )

    run_case(
        "set-default-before-load",
        COMMON
        + r'''
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
duckdb.set_default_connection(con)
print("default_is_con", duckdb.default_connection() is con)
report("duckdb.sql LOAD", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
report(
    "duckdb.sql CREATE VIEW",
    lambda: duckdb.sql("CREATE OR REPLACE VIEW spur_probe_view AS SELECT * FROM spur_probe(n := 2)"),
)
report("duckdb.sql direct SELECT", lambda: duckdb.sql("SELECT * FROM spur_probe()").fetchall())
report("duckdb.sql view SELECT", lambda: duckdb.sql("SELECT * FROM spur_probe_view").fetchall())
''',
    )

    run_case(
        "recommended-setup-then-user-cell",
        COMMON
        + r'''
_SPUR_DUCKDB_CONNECTION = duckdb.connect(
    database=":memory:",
    config={"allow_unsigned_extensions": "true"},
)
duckdb.set_default_connection(_SPUR_DUCKDB_CONNECTION)
report("setup LOAD", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
report(
    "setup CREATE VIEW",
    lambda: duckdb.sql("CREATE OR REPLACE VIEW spur_probe_view AS SELECT * FROM spur_probe(n := 2)"),
)

def user_cell():
    import duckdb

    return {
        "direct": duckdb.sql("SELECT * FROM spur_probe(n := 3)").fetchall(),
        "view": duckdb.sql("SELECT * FROM spur_probe_view").fetchall(),
    }

report("later user cell", user_cell)
''',
    )

    run_case(
        "recommended-rerun-reuses-connection",
        COMMON
        + r'''
def setup_cell():
    global _SPUR_DUCKDB_CONNECTION
    if "_SPUR_DUCKDB_CONNECTION" not in globals():
        _SPUR_DUCKDB_CONNECTION = duckdb.connect(
            database=":memory:",
            config={"allow_unsigned_extensions": "true"},
        )
    duckdb.set_default_connection(_SPUR_DUCKDB_CONNECTION)
    duckdb.sql(f"LOAD '{ext_sql}'")
    duckdb.sql("CREATE OR REPLACE VIEW spur_probe_view AS SELECT * FROM spur_probe(n := 2)")

setup_cell()
first_id = id(_SPUR_DUCKDB_CONNECTION)
duckdb.sql("CREATE TEMP TABLE user_tmp AS SELECT 42 AS x")
setup_cell()
second_id = id(_SPUR_DUCKDB_CONNECTION)
print("same_connection_object", first_id == second_id)
report("user temp table survived setup rerun", lambda: duckdb.sql("SELECT * FROM user_tmp").fetchall())
report("managed view after rerun", lambda: duckdb.sql("SELECT * FROM spur_probe_view").fetchall())
''',
    )

    run_case(
        "repeat-load-same-connection",
        COMMON
        + r'''
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
duckdb.set_default_connection(con)
report("first LOAD", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
report("second LOAD", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
report("SELECT after repeat LOAD", lambda: duckdb.sql("SELECT * FROM spur_probe(n := 1)").fetchall())
''',
    )

    run_case(
        "set-default-after-load-on-con",
        COMMON
        + r'''
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
report("con.execute LOAD", lambda: (con.execute(f"LOAD '{ext_sql}'"), "loaded")[1])
duckdb.set_default_connection(con)
print("default_is_con", duckdb.default_connection() is con)
report("module duckdb.sql SELECT", lambda: duckdb.sql("SELECT * FROM spur_probe(n := 4)").fetchall())
''',
    )

    run_case(
        "replace-after-prior-default-use",
        COMMON
        + r'''
print("prior_default_query", duckdb.sql("SELECT 1").fetchall())
old_default = duckdb.default_connection()
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
duckdb.set_default_connection(con)
print("old_default_is_current", duckdb.default_connection() is old_default)
print("new_default_is_con", duckdb.default_connection() is con)
report("duckdb.sql LOAD", lambda: duckdb.sql(f"LOAD '{ext_sql}'"))
report("duckdb.sql SELECT", lambda: duckdb.sql("SELECT * FROM spur_probe(n := 1)").fetchall())
''',
    )


if __name__ == "__main__":
    main()
