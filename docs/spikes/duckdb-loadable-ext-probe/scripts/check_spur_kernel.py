#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import sys


spec = importlib.util.find_spec("duckdb")
if spec is None:
    print(f"python {sys.executable}")
    print("duckdb not installed")
else:
    import duckdb

    print(f"python {sys.executable}")
    print(f"duckdb {duckdb.__version__}")

