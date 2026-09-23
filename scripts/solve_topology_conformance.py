#!/usr/bin/env python3
"""
Re-solve the spur-utilities topology model against the code as it exists now.

Loads the binding hard rules of the rev-2 decoupling spec from the pinned
receipt `sol_3a65e72044b7461d` (soft preferences dropped), observes crate
edges from `cargo metadata` plus a few code facts, fixes the observed values
as named hard constraints, and runs the Z3 solver through the stdio MCP server
`spur solver mcp`. No MCP-capable client is required.

Modes:
  incremental  fix only facts observed true (present edges, completed work).
               Missing target edges stay free: mid-migration states pass as
               long as nothing present contradicts the design.
  final        fix every observed fact, true or false. Passes only when the
               implemented topology satisfies every hard rule.

Usage:
  python3 scripts/solve_topology_conformance.py                  # incremental
  python3 scripts/solve_topology_conformance.py --mode final
  python3 scripts/solve_topology_conformance.py --assume m_acp=true   # probe a violation

Persisted solves land in the main worktree's `.spur/solver/` (see
--solver-root), so the brain can reload them with `get_solve_result`.

Exit codes:
  0  sat    — the observed code is consistent with the design
  1  unsat  — contradiction; the printed unsat_core names the rules and facts
  2  inconclusive (unknown / timeout / error / invalid request) — never a pass
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

BASE_SOLVE_ID = "sol_3a65e72044b7461d"

PACKAGES = {
    "tui": "spur-tui",
    "m": "spur-mentions",
    "c": "spur-commands",
    "u": "spur-utilities",
    "g": "spur-graph",
    "mcp": "spur-mcp",
    "acp": "spur-acp",
}

# Model variable -> (source key, target key). Only normal (non-dev, non-build)
# dependencies count; optional dependencies count because they are real edges
# once their feature is enabled.
EDGE_VARS = {
    "tui_m": ("tui", "m"),
    "tui_c": ("tui", "c"),
    "tui_u": ("tui", "u"),
    "u_m": ("u", "m"),
    "u_c": ("u", "c"),
    "u_acp": ("u", "acp"),
    "u_tui": ("u", "tui"),
    "m_acp": ("m", "acp"),
    "m_g": ("m", "g"),
    "m_tui": ("m", "tui"),
    "m_u": ("m", "u"),
    "c_acp": ("c", "acp"),
    "c_m": ("c", "m"),
    "c_tui": ("c", "tui"),
    "c_u": ("c", "u"),
    "g_mcp": ("g", "mcp"),
    "g_acp": ("g", "acp"),
    "mcp_acp": ("mcp", "acp"),
}

MOVED_REGISTRY = Path("crates/spur-utilities/commands/src/registry.rs")
TUI_REGISTRY = Path("crates/spur-tui/src/commands/registry.rs")
CI_WORKFLOW = Path(".github/workflows/ci.yml")

INCONCLUSIVE = {"unknown", "timeout", "error", "ended"}


# ---------------------------------------------------------------- pure helpers


def observe_edges(metadata: dict[str, Any]) -> dict[str, bool]:
    """Observed direct edges for every variable whose source package exists."""
    by_name = {p["name"]: p for p in metadata.get("packages", [])}
    deps: dict[str, dict[str, dict[str, Any]]] = {}
    for key, name in PACKAGES.items():
        pkg = by_name.get(name)
        if pkg is None:
            continue
        deps[key] = {
            d["name"]: d
            for d in pkg.get("dependencies", [])
            if d.get("kind") in (None, "normal")
        }

    observed: dict[str, bool] = {}
    for var, (src, dst) in EDGE_VARS.items():
        if src in deps:
            observed[var] = PACKAGES[dst] in deps[src]
    if "tui" in deps:
        mentions_dep = deps["tui"].get(PACKAGES["m"])
        observed["tui_code"] = bool(mentions_dep) and "code" in mentions_dep.get("features", [])
    return observed


def _production_region(text: str) -> str:
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.startswith("#[cfg(test)]"):
            return "\n".join(lines[:i])
    return text


def observe_local_injected(registry_text: str) -> bool | None:
    """True once the registry takes an injected `LocalLayer`; None if undecidable."""
    production = _production_region(registry_text)
    if "SpurLocalSource" in production:
        return False
    if "LocalLayer" in production:
        return True
    return None


def select_fixed(observed: dict[str, bool], mode: str) -> dict[str, bool]:
    if mode == "incremental":
        return {k: v for k, v in observed.items() if v}
    if mode == "final":
        return dict(observed)
    raise ValueError(f"unknown mode {mode!r}; expected 'incremental' or 'final'")


def _literal(name: str, value: bool) -> dict[str, Any]:
    var = {"kind": "var", "name": name}
    return var if value else {"kind": "op", "op": "not", "args": [var]}


def build_request(
    base: dict[str, Any],
    fixed: dict[str, bool],
    assumptions: dict[str, bool],
) -> dict[str, Any]:
    """Hard rules of `base` plus one named unit constraint per fixed fact."""
    declared = {v["name"] for v in base["vars"]}
    for name in [*fixed, *assumptions]:
        if name not in declared:
            raise KeyError(f"{name!r} is not a variable of {BASE_SOLVE_ID}")
    request = json.loads(json.dumps(base))
    request["constraints"] = [
        c for c in request["constraints"]
        if not c.get("soft") and not c.get("id", "").startswith("cex_")
    ]
    request["objectives"] = []
    for name, value in fixed.items():
        request["constraints"].append({"id": f"obs_{name}", "expr": _literal(name, value)})
    for name, value in assumptions.items():
        request["constraints"].append({"id": f"assume_{name}", "expr": _literal(name, value)})
    return request


def parse_assume(raw: str) -> tuple[str, bool]:
    name, sep, value = raw.partition("=")
    if not sep or not name or value.lower() not in ("true", "false"):
        raise ValueError(f"expected VAR=true|false, got {raw!r}")
    return name, value.lower() == "true"


def exit_code(result: dict[str, Any]) -> int:
    status = result.get("status")
    if status == "sat":
        return 0
    if status == "unsat":
        return 1
    return 2


# ---------------------------------------------------------------- observation


def _git(args: list[str], cwd: Path) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout.strip()


def repo_root() -> Path:
    return Path(_git(["rev-parse", "--show-toplevel"], Path.cwd()))


def main_worktree_root(root: Path) -> Path:
    common = Path(_git(["rev-parse", "--path-format=absolute", "--git-common-dir"], root))
    return common.parent


def cargo_metadata(root: Path) -> dict[str, Any]:
    env = {**os.environ, "SPUR_REMOTE": "0"}
    out = subprocess.run(
        [str(root / "scripts/spur-cargo"), "metadata", "--format-version", "1", "--no-deps"],
        cwd=root, env=env, check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def observe(root: Path) -> dict[str, bool]:
    observed = observe_edges(cargo_metadata(root))

    moved = (root / MOVED_REGISTRY).is_file()
    observed["reg_in_commands"] = moved
    registry = root / (MOVED_REGISTRY if moved else TUI_REGISTRY)
    if registry.is_file():
        injected = observe_local_injected(registry.read_text())
        if injected is not None:
            observed["local_injected"] = injected

    ci = root / CI_WORKFLOW
    if ci.is_file():
        text = ci.read_text()
        observed["gate_m_nodefault"] = any(
            "spur-mentions" in line and "--no-default-features" in line
            for line in text.splitlines()
        )
    return observed


# ---------------------------------------------------------------- solver RPC


class SolverSession:
    """Minimal JSON-RPC client for `spur solver mcp` over stdio."""

    def __init__(self, solver_root: Path) -> None:
        self._proc = subprocess.Popen(
            ["spur", "solver", "mcp", "--root", str(solver_root)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True,
        )
        self._next_id = 0
        self._rpc("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "solve_topology_conformance", "version": "1"},
        })
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, message: dict[str, Any]) -> None:
        assert self._proc.stdin is not None
        self._proc.stdin.write(json.dumps(message) + "\n")
        self._proc.stdin.flush()

    def _rpc(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self._next_id += 1
        request_id = self._next_id
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        assert self._proc.stdout is not None
        for line in self._proc.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RuntimeError(f"{method}: {message['error']}")
            return message["result"]
        raise RuntimeError(f"{method}: solver server exited without a response")

    def call(self, tool: str, arguments: dict[str, Any]) -> dict[str, Any]:
        result = self._rpc("tools/call", {"name": tool, "arguments": arguments})
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise RuntimeError(f"{tool}: {text}")
        return json.loads(text)

    def close(self) -> None:
        if self._proc.stdin:
            self._proc.stdin.close()
        try:
            self._proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self._proc.kill()


# ---------------------------------------------------------------- main


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").split("\n\n")[0])
    parser.add_argument("--mode", choices=("incremental", "final"), default="incremental")
    parser.add_argument("--assume", action="append", default=[], metavar="VAR=BOOL",
                        help="add a hypothetical fact (for probing violations)")
    parser.add_argument("--solver-root", type=Path,
                        help="where receipts live (default: SPUR_SOLVER_ROOT or main worktree)")
    parser.add_argument("--no-persist", action="store_true")
    parser.add_argument("--print-request", action="store_true")
    args = parser.parse_args(argv)

    root = repo_root()
    solver_root = args.solver_root or Path(
        os.environ.get("SPUR_SOLVER_ROOT") or main_worktree_root(root)
    )
    try:
        assumptions = dict(parse_assume(raw) for raw in args.assume)
        observed = observe(root)
    except (ValueError, subprocess.CalledProcessError) as err:
        print(f"error: {err}", file=sys.stderr)
        return 2

    session = SolverSession(solver_root)
    try:
        base = session.call("get_solve_result", {"solve_id": BASE_SOLVE_ID})["request"]
        request = build_request(base, select_fixed(observed, args.mode), assumptions)
        request["persist"] = not args.no_persist
        if args.print_request:
            print(json.dumps(request, indent=2))
        check = session.call("solve_constraint_check", request)
        if not check.get("valid"):
            print(json.dumps({"status": "error", "check": check}, indent=2))
            return 2
        result = session.call("solve_constraints", request)
    except (KeyError, RuntimeError) as err:
        print(f"error: {err}", file=sys.stderr)
        return 2
    finally:
        session.close()

    summary = {
        "mode": args.mode,
        "status": result.get("status"),
        "solve_id": result.get("solve_id"),
        "unsat_core": result.get("unsat_core"),
        "fixed": select_fixed(observed, args.mode),
        "assumed": assumptions,
    }
    print(json.dumps(summary, indent=2))
    code = exit_code(result)
    verdict = {0: "CONSISTENT", 1: "CONTRADICTION", 2: "INCONCLUSIVE"}[code]
    print(f"{verdict}: status={summary['status']} solve_id={summary['solve_id']}", file=sys.stderr)
    return code


if __name__ == "__main__":
    sys.exit(main())
