#!/usr/bin/env python3
"""Jev -> spur-solver pre-compiler POC probe.

Grounding probe for design epic bd-1yv63 (Jev as typesafe pre-compiler -> Z3).

What it does:
  1. For each test case, builds a structured `state` (user intent paraphrase +
     candidate rule catalog cards + fixture data) and a typed question battery
     (Choice for routing/mode/binding, Noul for completeness/calibration).
  2. Calls the real TypeSafe/Jev HTTP API once per case.
  3. Deterministically compiles the typed answers into a spur-solver request
     (solve_rules for family rules, solve_constraints for the generic B' case)
     and writes each compiled request to /tmp/jev_poc/.
  4. Emits an evaluation summary (routing accuracy, confidence, calibration,
     latency, token usage).

Ground truth comes from crates/spur-solver conformance fixtures:
  - design/layout.containment        valid + one-unit-overflow scenes
  - scheduling.precedence_finish_start valid facts

The compiled requests are then piped through the real MCP tools
(solve_constraint_check / solve_rules / solve_constraints) by the operator.

Usage:
  set -a; source .env; set +a
  python3 scripts/jev_poc_probe.py [--out /tmp/jev_poc]
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

API_URL = "https://api.typesafe.ai/v1/systemone"
MODEL = "jev-latest"
CONFIDENCE_GATE = 0.60  # probe-level gate: below this we would refuse to solve
NOUL_GATE = 0.50

# ---------------------------------------------------------------- catalog ----
# Candidate rule cards: id -> one-line summary (verbatim from YAML manifests).
RULE_CARDS = {
    "layout.containment": "Keep one axis-aligned rectangle inside another with optional padding.",
    "layout.non_overlap": "Separate two axis-aligned rectangles by an optional minimum gap.",
    "scheduling.precedence_finish_start": "Require a predecessor to finish plus a nonnegative lag before its successor starts.",
    "scheduling.cumulative_capacity": "Keep every declared resource's concurrent demand within one machine's capacity at every horizon tick.",
    "resource.quota_capacity": "Require aggregate replica demand to fit each selected quota capacity.",
    "data_integrity.unique": "Require active rows with complete keys to differ on at least one key field.",
}

MODE_CARD = {
    "verify": "A complete model is supplied; evaluate every selected rule against it.",
    "synthesize": "Bounded unknowns must be completed under every selected rule.",
}

# Rule ID -> family ID. Rule-id prefixes are NOT family ids (layout.* lives in
# the `design` family); the mapping is catalog metadata, never inference.
RULE_FAMILY = {
    "layout.containment": "design",
    "layout.non_overlap": "design",
    "scheduling.precedence_finish_start": "scheduling",
    "scheduling.cumulative_capacity": "scheduling",
    "resource.quota_capacity": "resource",
    "data_integrity.unique": "data_integrity",
}

# ------------------------------------------------------------------ cases ----

DESIGN_SCENE_VALID = {
    "viewport": {"width": 390, "height": 844},
    "nodes": {
        "parent": {"rect": {"x": 0, "y": 0, "width": 100, "height": 100}},
        "child": {"rect": {"x": 76, "y": 0, "width": 24, "height": 24}},
    },
}

DESIGN_SCENE_OVERFLOW = {
    "viewport": {"width": 390, "height": 844},
    "nodes": {
        "parent": {"rect": {"x": 0, "y": 0, "width": 100, "height": 100}},
        "child": {"rect": {"x": 77, "y": 0, "width": 24, "height": 24}},
    },
}

SCHED_FACTS_VALID = {
    "horizon": 5,
    "jobs": {
        "a": {
            "release": 0, "deadline": 5, "durations": {"m1": 2},
            "eligible_machines": ["m1"], "demands": {"cpu": 1},
            "assignment": {"machine": "m1", "start": 0},
        },
        "b": {
            "release": 0, "deadline": 5, "durations": {"m2": 2},
            "eligible_machines": ["m2"], "demands": {"cpu": 1},
            "assignment": {"machine": "m2", "start": 2},
        },
    },
    "machines": {
        "m1": {"capacities": {"cpu": 1}},
        "m2": {"capacities": {"cpu": 1}},
    },
    "precedence": [{"before": "a", "after": "b", "minimum_lag": 0}],
}


def routing_questions(extra: dict | None = None) -> dict:
    q = {
        "route_rule": {
            "type": "choice",
            "instructions": "Which solver rule best matches the user intent in `intent`?",
            "criteria": RULE_CARDS,
        },
        "solve_mode": {
            "type": "choice",
            "instructions": (
                "Should the solver VERIFY a fully-supplied model, or SYNTHESIZE "
                "missing bounded unknowns?"
            ),
            "criteria": MODE_CARD,
        },
        "data_complete": {
            "type": "noul",
            "instructions": (
                "Is every value required by the selected rule present and "
                "concrete in `data` (no placeholders, no missing subjects)?"
            ),
            "criteria": {
                "true": "All required subjects, parameters, and scene/facts values are concrete.",
                "false": "Something required by the rule is missing, vague, or symbolic.",
            },
        },
    }
    if extra:
        q.update(extra)
    return q


CASES = [
    {
        "id": "case1_design_containment_valid",
        "expect": {"rule": "layout.containment", "mode": "verify", "outcome": "pass"},
        "state": {
            "intent": (
                "QA pass on this layout: a 100x100 settings panel sits at the "
                "top-left corner of a 390x844 viewport, and a 24x24 icon tile is "
                "placed at x=76,y=0 inside it. Confirm the icon remains fully "
                "inside the panel with no inset margin. Everything is measured; "
                "check the current layout as-is."
            ),
            "data": DESIGN_SCENE_VALID,
        },
        "questions": routing_questions(),
    },
    {
        "id": "case2_scheduling_precedence_valid",
        "expect": {"rule": "scheduling.precedence_finish_start", "mode": "verify",
                   "outcome": "pass", "predecessor": "a"},
        "state": {
            "intent": (
                "Two jobs on a 5-tick horizon: job a runs on machine m1 for 2 "
                "ticks starting at 0; job b runs on machine m2 for 2 ticks "
                "starting at tick 2. Rule: a has to be completely done before b "
                "may start, with zero slack between them. Audit the schedule."
            ),
            "data": SCHED_FACTS_VALID,
        },
        "questions": routing_questions({
            "predecessor": {
                "type": "choice",
                "instructions": "Which job is the PREDECESSOR (must finish first)?",
                "criteria": {"a": "Job a finishes first.", "b": "Job b finishes first."},
            },
        }),
    },
    {
        "id": "case3_design_containment_overflow",
        "expect": {"rule": "layout.containment", "mode": "verify",
                   "outcome": "fail", "diagnostic": "design.outside_parent"},
        "state": {
            "intent": (
                "QA pass on this layout: a 100x100 settings panel sits at the "
                "top-left corner of a 390x844 viewport, and a 24x24 icon tile is "
                "placed at x=77,y=0 inside it. Confirm the icon remains fully "
                "inside the panel with no inset margin. Everything is measured; "
                "check the current layout as-is."
            ),
            "data": DESIGN_SCENE_OVERFLOW,
        },
        "questions": routing_questions(),
    },
    {
        "id": "case4_generic_bprime",
        "expect": {"var_kind": "int_range", "op": "ge", "hard": True, "status": "sat"},
        "state": {
            "intent": (
                "Capacity floor check for one knob: x is an integer setting "
                "allowed from 0 through 10. Throughput is 3 units per x and the "
                "floor requirement is at least 12 units. Is there a legal value "
                "of x that meets the floor? This is a hard requirement, not a "
                "preference."
            ),
            "data": {"x_domain": [0, 10], "throughput_per_x": 3, "floor": 12},
        },
        "questions": {
            "var_x_kind": {
                "type": "choice",
                "instructions": "Which typed variable kind should represent `x`?",
                "criteria": {
                    "int_range": "Bounded integer with inclusive min/max.",
                    "int": "Unbounded integer.",
                    "bool": "Boolean flag.",
                    "enum": "Finite label set.",
                    "real": "Unbounded real.",
                    "bit_vec": "Fixed-width bit-vector.",
                },
            },
            "comparison_op": {
                "type": "choice",
                "instructions": "Which comparison relates 3*x to the floor 12?",
                "criteria": {
                    "ge": "3*x must be greater than or equal to 12.",
                    "gt": "3*x must be strictly greater than 12.",
                    "le": "3*x must be at most 12.",
                    "lt": "3*x must be strictly below 12.",
                    "eq": "3*x must equal 12.",
                    "ne": "3*x must differ from 12.",
                },
            },
            "is_hard": {
                "type": "noul",
                "instructions": "Is this requirement a hard constraint (not a soft preference)?",
            },
        },
    },
    {
        "id": "case5_ambiguity_calibration",
        "expect": {"gate": "blocked"},  # want low data_complete / low confidence
        "state": {
            "intent": "Make sure the layout looks right and nothing overlaps weirdly.",
            "data": {"viewport": {"width": 390, "height": 844}},
        },
        "questions": routing_questions(),
    },
]

# ------------------------------------------------------------- jev client ----


def call_jev(state: dict, questions: dict) -> tuple[dict, float]:
    payload = {"state": state, "model": MODEL, "questions": questions}
    req = urllib.request.Request(
        API_URL,
        data=json.dumps(payload).encode(),
        headers={
            "Authorization": f"Bearer {os.environ['JEV_API_KEY']}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    started = time.perf_counter()
    with urllib.request.urlopen(req, timeout=30) as resp:
        body = json.loads(resp.read())
    return body, (time.perf_counter() - started) * 1000.0


# ------------------------------------------------------------- compilers ----


def compile_family_request(case: dict, answers: dict) -> dict:
    route = answers["route_rule"]
    mode = answers["solve_mode"]["choice"]
    rule_id = route["choice"]
    family = RULE_FAMILY[rule_id]
    req = {
        "family": family,
        "mode": mode,
        "rules": [{"rule_id": rule_id, "subjects": []}],
    }
    if family == "design":
        req["rules"][0]["subjects"] = ["child", "parent"]
        req["scene"] = case["state"]["data"]
        req["unknowns"] = []
    elif family == "scheduling":
        pred = answers.get("predecessor", {}).get("choice", "a")
        succ = "b" if pred == "a" else "a"
        req["rules"][0]["subjects"] = [pred, succ]
        req["facts"] = case["state"]["data"]
        req["unknowns"] = []
    return req


def compile_bprime_request(answers: dict) -> dict:
    kind = answers["var_x_kind"]["choice"]
    op = answers["comparison_op"]["choice"]
    hard = answers["is_hard"]["noul"] >= NOUL_GATE
    var = {"type": kind, "name": "x"}
    if kind == "int_range":
        var.update({"min": 0, "max": 10})
    constraint = {
        "id": "throughput_floor",
        "expr": {
            "kind": "op", "op": op,
            "args": [
                {"kind": "op", "op": "mul",
                 "args": [{"kind": "int", "value": 3}, {"kind": "var", "name": "x"}]},
                {"kind": "int", "value": 12},
            ],
        },
    }
    if not hard:
        constraint.update({"soft": True, "weight": 1})
    return {"vars": [var], "constraints": [constraint]}


# ----------------------------------------------------------------- main ----


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="/tmp/jev_poc")
    parser.add_argument("--only", default=None, help="run a single case id")
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)

    if "JEV_API_KEY" not in os.environ:
        print("JEV_API_KEY not set", file=sys.stderr)
        return 2

    summary = []
    for case in CASES:
        if args.only and case["id"] != args.only:
            continue
        card = {"case": case["id"], "expect": case["expect"]}
        try:
            body, latency_ms = call_jev(case["state"], case["questions"])
        except urllib.error.HTTPError as err:
            card.update({"error": f"HTTP {err.code}: {err.read().decode()[:300]}"})
            summary.append(card)
            continue
        answers = body["answers"]
        card["model"] = body.get("model")
        card["latency_ms"] = round(latency_ms, 1)
        card["usage"] = body.get("usage")

        if "route_rule" in answers:
            r = answers["route_rule"]
            card["routed"] = {"choice": r["choice"], "confidence": r["confidence"],
                              "probabilities": r["probabilities"]}
            card["mode"] = answers["solve_mode"]["choice"]
            card["mode_conf"] = answers["solve_mode"]["confidence"]
            card["data_complete_noul"] = answers["data_complete"]["noul"]
            if "predecessor" in answers:
                card["predecessor"] = {
                    "choice": answers["predecessor"]["choice"],
                    "confidence": answers["predecessor"]["confidence"],
                    "probabilities": answers["predecessor"]["probabilities"],
                }
            compiled = compile_family_request(case, answers)
            # Gate on EVERY decision: routing, mode, any binding choices,
            # and data completeness. One uncertain decision blocks the solve.
            decision_confidences = [
                r["confidence"],
                answers["solve_mode"]["confidence"],
                answers["data_complete"]["noul"],
            ]
            if "predecessor" in answers:
                decision_confidences.append(answers["predecessor"]["confidence"])
            weakest = min(decision_confidences)
            card["weakest_decision"] = round(weakest, 3)
            gate_open = weakest >= CONFIDENCE_GATE
            card["gate"] = "open" if gate_open else "blocked"
        elif "var_x_kind" in answers:
            card["var_kind"] = answers["var_x_kind"]["choice"]
            card["var_kind_conf"] = answers["var_x_kind"]["confidence"]
            card["op"] = answers["comparison_op"]["choice"]
            card["op_conf"] = answers["comparison_op"]["confidence"]
            card["hard_noul"] = answers["is_hard"]["noul"]
            compiled = compile_bprime_request(answers)
            weakest = min(
                answers["var_x_kind"]["confidence"],
                answers["comparison_op"]["confidence"],
                answers["is_hard"]["noul"],
            )
            card["weakest_decision"] = round(weakest, 3)
            card["gate"] = "open" if weakest >= CONFIDENCE_GATE else "blocked"
        else:
            card["error"] = "unexpected answer shape"
            summary.append(card)
            continue

        card["compiled"] = compiled
        path = os.path.join(args.out, f"{case['id']}.json")
        with open(path, "w") as fh:
            json.dump(compiled, fh, indent=2)
        card["compiled_path"] = path
        summary.append(card)

    print(json.dumps(summary, indent=2))
    with open(os.path.join(args.out, "summary.json"), "w") as fh:
        json.dump(summary, fh, indent=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
