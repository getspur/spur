#!/usr/bin/env python3
from __future__ import annotations

import sys
from pathlib import Path

import duckdb


def sql_string(path: Path) -> str:
    return str(path).replace("'", "''")


def main() -> None:
    default_extension = (
        Path(__file__).resolve().parents[1]
        / "build"
        / "release"
        / "spur_probe.duckdb_extension"
    )
    extension_path = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else default_extension

    con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
    con.execute(f"LOAD '{sql_string(extension_path)}'")

    print("extension", extension_path)
    print("default", con.sql("SELECT * FROM spur_probe()").fetchall())
    print("named_colon", con.sql("SELECT * FROM spur_probe(n := 5)").fetchall())
    print("named_equals", con.sql("SELECT * FROM spur_probe(n = 2)").fetchall())
    print("duckdb", duckdb.__version__)


if __name__ == "__main__":
    main()

