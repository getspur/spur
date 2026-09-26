#!/usr/bin/env python3
"""Jev -> spur-solver pre-compiler POC probe, phase 2 (deeper evaluation).

Suites (design epic bd-1yv63):
  B  routing confusion matrix — 6 fixture-grounded cases across confusable
     families (design/non_overlap, scheduling/cumulative_capacity,
     resource/quota_capacity, data_integrity/unique, workflow/transition_allowed)
  C  mode decision — verify vs synthesize on scheduling.assignment_exactly_once
  D  generic B-prime depth — enum labels, hard/soft mix, objectives
  E  stability — 5 repeats of a mini-battery (routing + directional binding +
     hard-vs-soft), reporting choice stability and confidence spread
  F  fan-out scaling — one call routing all six suite-B intents (12 questions)
     vs six individual calls

Ground truth: crates/spur-solver conformance fixtures. Compiled requests are
written to /tmp/jev_poc2/ for piping through the real MCP solver tools.

Usage:
  set -a; source .env; set +a
  python3 scripts/jev_poc_probe2.py [--out /tmp/jev_poc2]
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
CONFIDENCE_GATE = 0.60

RULE_CARDS = {
    "layout.containment": "Keep one axis-aligned rectangle inside another with optional padding.",
    "layout.non_overlap": "Separate two axis-aligned rectangles by an optional minimum gap.",
    "scheduling.precedence_finish_start": "Require a predecessor to finish plus a nonnegative lag before its successor starts.",
    "scheduling.cumulative_capacity": "Keep every declared resource's concurrent demand within one machine's capacity at every horizon tick.",
    "scheduling.assignment_exactly_once": "Require a job to occupy exactly one bounded placement (machine and start tick).",
    "resource.quota_capacity": "Require aggregate replica demand to fit each selected quota capacity.",
    "data_integrity.unique": "Require active rows with complete keys to differ on at least one key field.",
    "workflow.transition_allowed": "Require every observed trace step to follow an enabled state transition.",
}

RULE_FAMILY = {
    "layout.containment": "design",
    "layout.non_overlap": "design",
    "scheduling.precedence_finish_start": "scheduling",
    "scheduling.cumulative_capacity": "scheduling",
    "scheduling.assignment_exactly_once": "scheduling",
    "resource.quota_capacity": "resource",
    "data_integrity.unique": "data_integrity",
    "workflow.transition_allowed": "workflow",
}

MODE_CARD = {
    "verify": "A complete model is supplied; evaluate every selected rule against it.",
    "synthesize": "Bounded unknowns must be completed under every selected rule.",
}


def call_jev(state, questions):
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
    with urllib.request.urlopen(req, timeout=45) as resp:
        body = json.loads(resp.read())
    return body, (time.perf_counter() - started) * 1000.0


def routing_battery(extra=None, route_instruction="Which solver rule best matches the user intent in `intent`?"):
    q = {
        "route_rule": {
            "type": "choice",
            "instructions": route_instruction,
            "criteria": RULE_CARDS,
        },
        "solve_mode": {
            "type": "choice",
            "instructions": "Should the solver VERIFY a fully-supplied model, or SYNTHESIZE missing bounded unknowns?",
            "criteria": MODE_CARD,
        },
        "data_complete": {
            "type": "noul",
            "instructions": "Is every value required by the selected rule present and concrete in `data` (no placeholders, no missing subjects)?",
            "criteria": {
                "true": "All required subjects, parameters, and scene/facts values are concrete.",
                "false": "Something required by the rule is missing, vague, or symbolic.",
            },
        },
    }
    if extra:
        q.update(extra)
    return q


# --------------------------------------------------------------- fixtures ----

SCENE_NON_OVERLAP_VALID = {
    "viewport": {"width": 390, "height": 844},
    "nodes": {
        "first": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
        "second": {"rect": {"x": 48, "y": 0, "width": 24, "height": 24}},
    },
}
SCENE_NON_OVERLAP_INVALID = {
    "viewport": {"width": 390, "height": 844},
    "nodes": {
        "first": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
        "second": {"rect": {"x": 47, "y": 0, "width": 24, "height": 24}},
    },
}

SCHED_CUMULATIVE_FACTS = {
    "horizon": 4,
    "jobs": {
        "a": {"release": 0, "deadline": 4, "durations": {"m1": 2}, "eligible_machines": ["m1"],
              "demands": {"cpu": 1}, "assignment": {"machine": "m1", "start": 0}},
        "b": {"release": 0, "deadline": 4, "durations": {"m1": 2}, "eligible_machines": ["m1"],
              "demands": {"cpu": 1}, "assignment": {"machine": "m1", "start": 2}},
    },
    "machines": {"m1": {"capacities": {"cpu": 1}}},
    "precedence": [],
}

QUOTA_FACTS = {
    "workloads": {
        "api": {"replicas": 3, "requests": {"cpu": 500}, "limits": {"cpu": 500}, "domain_counts": {}},
    },
    "pools": {},
    "quotas": {"team": {"resources": {"cpu": 1500}}},
}

UNIQUE_FACTS = {
    "relations": {
        "records": {
            "fields": {"key": {"kind": "integer", "minimum": 0, "maximum": 1000}},
            "rows": {
                "first": {"active": True, "cells": {"key": {"present": True, "value": 1}}},
                "second": {"active": True, "cells": {"key": {"present": True, "value": 2}}},
                "null_key": {"active": True, "cells": {"key": {"present": False, "value": None}}},
            },
        },
    },
    "unique_constraints": {"record_key": {"relation": "records", "fields": ["key"]}},
    "foreign_keys": {}, "cardinality_constraints": {}, "value_ranges": {},
    "conditional_requirements": {}, "aggregate_balances": {}, "consistency_relations": {},
    "temporal_constraints": {},
}

WORKFLOW_FACTS = {
    "horizon": 2,
    "state_domain": ["Draft", "Review", "Approved", "Rejected"],
    "event_domain": ["submit", "approve", "reject"],
    "initial_states": ["Draft"],
    "safe_states": ["Draft", "Review", "Approved"],
    "target_states": ["Approved"],
    "enabled_transitions": [
        {"step": 0, "from": "Draft", "event": "submit", "to": "Review"},
        {"step": 1, "from": "Review", "event": "approve", "to": "Approved"},
        {"step": 1, "from": "Review", "event": "reject", "to": "Rejected"},
    ],
    "traces": {"approval": {"states": ["Draft", "Review", "Approved"], "events": ["submit", "approve"]}},
}

ASSIGN_SYNTH_FACTS = {
    "horizon": 4,
    "jobs": {
        "a": {"release": 0, "deadline": 4, "durations": {"m1": 2, "m2": 2},
              "eligible_machines": ["m1", "m2"], "demands": {"cpu": 1}, "assignment": None},
    },
    "machines": {"m1": {"capacities": {"cpu": 1}}, "m2": {"capacities": {"cpu": 1}}},
    "precedence": [],
}

ASSIGN_VERIFY_FACTS = {
    "horizon": 4,
    "jobs": {
        "a": {"release": 0, "deadline": 4, "durations": {"m1": 2, "m2": 2},
              "eligible_machines": ["m1", "m2"], "demands": {"cpu": 1},
              "assignment": {"machine": "m1", "start": 0}},
    },
    "machines": {"m1": {"capacities": {"cpu": 1}}, "m2": {"capacities": {"cpu": 1}}},
    "precedence": [],
}

SUITE_B = [
    {
        "id": "B1_non_overlap_valid",
        "expect": {"rule": "layout.non_overlap", "mode": "verify", "outcome": "pass"},
        "state": {
            "intent": ("On one row of a 390x844 viewport, a 24x24 notification badge occupies "
                       "x=0..24 and a 24x24 avatar occupies x=48..72. The two must never cover "
                       "each other and need at least 24 units of breathing room between them. "
                       "Audit the current row."),
            "data": SCENE_NON_OVERLAP_VALID,
        },
    },
    {
        "id": "B2_non_overlap_violation",
        "expect": {"rule": "layout.non_overlap", "mode": "verify", "outcome": "fail",
                   "diagnostic": "design.overlap"},
        "state": {
            "intent": ("On one row of a 390x844 viewport, a 24x24 notification badge occupies "
                       "x=0..24 and a 24x24 avatar occupies x=47..71. The two must never cover "
                       "each other and need at least 24 units of breathing room between them. "
                       "Audit the current row."),
            "data": SCENE_NON_OVERLAP_INVALID,
        },
    },
    {
        "id": "B3_cumulative_capacity_valid",
        "expect": {"rule": "scheduling.cumulative_capacity", "mode": "verify", "outcome": "pass"},
        "state": {
            "intent": ("One machine m1 with a single cpu slot over a 4-tick horizon. Job a "
                       "occupies ticks 0-2 and job b occupies ticks 2-4; each needs 1 cpu. The "
                       "machine must never run two jobs at the same tick. Audit the plan."),
            "data": SCHED_CUMULATIVE_FACTS,
        },
    },
    {
        "id": "B4_quota_capacity_valid",
        "expect": {"rule": "resource.quota_capacity", "mode": "verify", "outcome": "pass",
                   "dimension": "cpu"},
        "state": {
            "intent": ("The team owns a quota of 1500 cpu. The api deployment runs 3 replicas "
                       "and each replica requests 500 cpu. Confirm the deployment fits inside "
                       "the team quota."),
            "data": QUOTA_FACTS,
        },
        "extra_questions": {
            "resource_dimension": {
                "type": "choice",
                "instructions": "Which resource dimension does the quota check apply to?",
                "criteria": {
                    "cpu": "Compute cores.", "memory": "RAM bytes.",
                    "storage": "Disk bytes.", "network": "Bandwidth.",
                },
            },
        },
    },
    {
        "id": "B5_unique_valid",
        "expect": {"rule": "data_integrity.unique", "mode": "verify", "outcome": "pass"},
        "state": {
            "intent": ("Records table audit: row 'first' has key 1, row 'second' has key 2, "
                       "row 'null_key' has no key at all. No two active records may share the "
                       "same key; a missing key is not a duplicate. Check the snapshot."),
            "data": UNIQUE_FACTS,
        },
    },
    {
        "id": "B6_transition_allowed_valid",
        "expect": {"rule": "workflow.transition_allowed", "mode": "verify", "outcome": "pass"},
        "state": {
            "intent": ("Approval flow with horizon 2: Draft goes to Review on submit; Review "
                       "goes to Approved on approve or Rejected on reject. Observed trace "
                       "'approval' visits [Draft, Review, Approved] via events [submit, "
                       "approve]. Every observed step must use an enabled transition. Audit it."),
            "data": WORKFLOW_FACTS,
        },
    },
]

SUITE_C = [
    {
        "id": "C1_assignment_synthesize",
        "expect": {"rule": "scheduling.assignment_exactly_once", "mode": "synthesize",
                   "outcome": "solution"},
        "state": {
            "intent": ("Job a is currently unscheduled over a 4-tick horizon. It takes 2 ticks "
                       "on m1 or 2 ticks on m2 and needs 1 cpu. It must end up with exactly one "
                       "placement (machine + start tick). Propose a valid placement."),
            "data": ASSIGN_SYNTH_FACTS,
        },
    },
    {
        "id": "C2_assignment_verify",
        "expect": {"rule": "scheduling.assignment_exactly_once", "mode": "verify", "outcome": "pass"},
        "state": {
            "intent": ("Job a is already scheduled on m1 starting at tick 0 over a 4-tick "
                       "horizon (2 ticks on either machine, 1 cpu). Confirm it is placed "
                       "exactly once."),
            "data": ASSIGN_VERIFY_FACTS,
        },
    },
]

# ----------------------------------------------------------------- compile ----


def compile_family(case, answers):
    rule_id = answers["route_rule"]["choice"]
    mode = answers["solve_mode"]["choice"]
    family = RULE_FAMILY[rule_id]
    data = case["state"]["data"]
    req = {"family": family, "mode": mode, "rules": [{"rule_id": rule_id, "subjects": []}]}

    if family == "design":
        req["rules"][0]["subjects"] = ["first", "second"]
        req["rules"][0]["parameters"] = {"minimum_gap": 24}
        req["scene"] = data
        req["unknowns"] = []
    elif family == "scheduling":
        if rule_id == "scheduling.cumulative_capacity":
            req["rules"][0]["subjects"] = ["m1"]
            req["rules"][0]["parameters"] = {}
        else:  # assignment_exactly_once
            req["rules"][0]["subjects"] = ["a"]
            req["rules"][0]["parameters"] = {}
        req["facts"] = data
        req["unknowns"] = []
        if mode == "synthesize":
            req["unknowns"] = [{"kind": "assignment", "job": "a"}]
    elif family == "resource":
        dim = answers.get("resource_dimension", {}).get("choice", "cpu")
        req["rules"][0]["subjects"] = ["team", "api"]
        req["rules"][0]["parameters"] = {"resources": [dim]}
        req["facts"] = data
        req["unknowns"] = []
    elif family == "data_integrity":
        req["rules"][0]["subjects"] = ["record_key"]
        req["rules"][0]["parameters"] = {}
        req["facts"] = data
        req["unknowns"] = []
    elif family == "workflow":
        req["rules"][0]["subjects"] = ["approval"]
        req["rules"][0]["parameters"] = {}
        req["facts"] = data
        req["unknowns"] = []
    return req


def weakest_of(answers, keys):
    vals = []
    for key in keys:
        ans = answers.get(key)
        if not ans:
            continue
        if ans["type"] == "noul":
            vals.append(ans["noul"])
        elif "confidence" in ans:
            vals.append(ans["confidence"])
    return min(vals) if vals else None


# ------------------------------------------------------------- suite D ----

D_CASES = [
    {
        "id": "D1_enum_label",
        "expect": {"y_kind": "enum", "label": "large", "status": "sat"},
        "state": {
            "intent": ("Two settings: x is an integer knob from 0 to 10, and tier is a size "
                       "class that can only be 'small' or 'large'. Requirements: doubled x "
                       "must reach at least 8, and the tier must be set to 'large'. Both are "
                       "hard requirements."),
            "data": {"x_domain": [0, 10], "tiers": ["small", "large"]},
        },
        "questions": {
            "y_kind": {
                "type": "choice",
                "instructions": "Which typed variable kind should represent the size class 'tier'?",
                "criteria": {
                    "enum": "Finite label set.", "int_range": "Bounded integer.",
                    "bool": "Boolean flag.", "int": "Unbounded integer.",
                    "real": "Unbounded real.", "bit_vec": "Fixed-width bit-vector.",
                },
            },
            "required_label": {
                "type": "choice",
                "instructions": "Which tier label does the intent require?",
                "criteria": {"small": "The small tier.", "large": "The large tier."},
            },
            "x_kind": {
                "type": "choice",
                "instructions": "Which typed variable kind should represent the knob 'x'?",
                "criteria": {
                    "int_range": "Bounded integer with inclusive min/max.",
                    "int": "Unbounded integer.", "bool": "Boolean flag.",
                    "enum": "Finite label set.", "real": "Unbounded real.",
                    "bit_vec": "Fixed-width bit-vector.",
                },
            },
        },
        "compile": lambda a: {
            "vars": [
                {"type": a["x_kind"]["choice"], "name": "x", "min": 0, "max": 10}
                if a["x_kind"]["choice"] == "int_range" else
                {"type": a["x_kind"]["choice"], "name": "x"},
                {"type": a["y_kind"]["choice"], "name": "tier",
                 **({"values": ["small", "large"]} if a["y_kind"]["choice"] == "enum" else {})},
            ],
            "constraints": [
                {"id": "doubled_x_floor", "expr": {
                    "kind": "op", "op": "ge",
                    "args": [
                        {"kind": "op", "op": "mul",
                         "args": [{"kind": "int", "value": 2}, {"kind": "var", "name": "x"}]},
                        {"kind": "int", "value": 8},
                    ],
                }},
                {"id": "tier_required", "expr": {
                    "kind": "enum_label", "var": "tier",
                    "label": a["required_label"]["choice"],
                }},
            ],
        },
        "decision_keys": ["y_kind", "required_label", "x_kind"],
    },
    {
        "id": "D2_hard_soft_objective",
        "expect": {"floor_hard": True, "cap_soft": True, "objective": "minimize", "optimum": 4},
        "state": {
            "intent": ("x is an integer from 0 to 10. Absolute requirement: tripled x must be "
                       "at least 12 — this one is non-negotiable. Preference (nice to have, "
                       "weight 1): keep x at or below 4. Also report the smallest legal x."),
            "data": {"x_domain": [0, 10]},
        },
        "questions": {
            "floor_is_hard": {
                "type": "noul",
                "instructions": "Is 'tripled x must be at least 12' a HARD requirement (not a preference)?",
            },
            "cap_is_soft": {
                "type": "noul",
                "instructions": "Is 'keep x at or below 4' a SOFT preference (violable at a cost), not a hard requirement?",
            },
            "objective": {
                "type": "choice",
                "instructions": "Which optimization objective matches 'report the smallest legal x'?",
                "criteria": {
                    "minimize": "Minimize x.", "maximize": "Maximize x.",
                    "none": "No objective needed.",
                },
            },
        },
        "compile": lambda a: {
            "vars": [{"type": "int_range", "name": "x", "min": 0, "max": 10}],
            "constraints": [
                {"id": "floor", "soft": not (a["floor_is_hard"]["noul"] >= 0.5),
                 "expr": {"kind": "op", "op": "ge",
                          "args": [{"kind": "op", "op": "mul",
                                    "args": [{"kind": "int", "value": 3}, {"kind": "var", "name": "x"}]},
                                   {"kind": "int", "value": 12}]}},
                {"id": "cap", "soft": a["cap_is_soft"]["noul"] >= 0.5, "weight": 1,
                 "expr": {"kind": "op", "op": "le",
                          "args": [{"kind": "var", "name": "x"}, {"kind": "int", "value": 4}]}},
            ],
            "objectives": ([{"op": "minimize", "expr": {"kind": "var", "name": "x"}}]
                           if a["objective"]["choice"] == "minimize" else []),
        },
        "decision_keys": ["floor_is_hard", "cap_is_soft", "objective"],
    },
    {
        "id": "D3_multi_constraint",
        "expect": {"sum_hard": True, "distinct_hard": True, "pref_soft": True, "weight": 2,
                   "status": "sat"},
        "state": {
            "intent": ("Two integers a and b, each 0..20. Non-negotiable: a + b must equal "
                       "exactly 20, and a must differ from b. Strong preference (weight 2): "
                       "push a up to at least 15. Find values that satisfy everything."),
            "data": {"domains": {"a": [0, 20], "b": [0, 20]}},
        },
        "questions": {
            "sum_is_hard": {
                "type": "noul",
                "instructions": "Is 'a + b must equal exactly 20' a HARD requirement?",
            },
            "distinct_is_hard": {
                "type": "noul",
                "instructions": "Is 'a must differ from b' a HARD requirement?",
            },
            "pref_is_soft": {
                "type": "noul",
                "instructions": "Is 'a at least 15' a SOFT preference (violable at a cost)?",
            },
            "pref_weight": {
                "type": "choice",
                "instructions": "What weight does 'strong preference' justify for the soft constraint?",
                "criteria": {"1": "Normal weight.", "2": "Strong weight.", "5": "Critical-almost-hard weight."},
            },
        },
        "compile": lambda a: {
            "vars": [
                {"type": "int_range", "name": "a", "min": 0, "max": 20},
                {"type": "int_range", "name": "b", "min": 0, "max": 20},
            ],
            "constraints": [
                {"id": "sum_twenty", "soft": not (a["sum_is_hard"]["noul"] >= 0.5),
                 "expr": {"kind": "op", "op": "eq",
                          "args": [{"kind": "op", "op": "add",
                                    "args": [{"kind": "var", "name": "a"}, {"kind": "var", "name": "b"}]},
                                   {"kind": "int", "value": 20}]}},
                {"id": "distinct", "soft": not (a["distinct_is_hard"]["noul"] >= 0.5),
                 "expr": {"kind": "op", "op": "ne",
                          "args": [{"kind": "var", "name": "a"}, {"kind": "var", "name": "b"}]}},
                {"id": "a_high", "soft": a["pref_is_soft"]["noul"] >= 0.5,
                 "weight": int(a["pref_weight"]["choice"]),
                 "expr": {"kind": "op", "op": "ge",
                          "args": [{"kind": "var", "name": "a"}, {"kind": "int", "value": 15}]}},
            ],
        },
        "decision_keys": ["sum_is_hard", "distinct_is_hard", "pref_is_soft", "pref_weight"],
    },
]

# ------------------------------------------------------------- suite E ----

E_MINI_BATTERY = {
    "route": {
        "type": "choice",
        "instructions": {
            "intent": "One machine m1 with a single cpu slot over a 4-tick horizon. Job a occupies ticks 0-2 and job b occupies ticks 2-4; each needs 1 cpu. The machine must never run two jobs at the same tick. Audit the plan.",
            "question": "Which solver rule best matches `intent`?",
        },
        "criteria": RULE_CARDS,
    },
    "same_direction": {
        "type": "noul",
        "instructions": {
            "sentence": "The user requirement is: a has to be completely done before b may start.",
            "edge": {"declared_precedence_edge": {"before": "a", "after": "b"}},
            "question": "Does `declared_precedence_edge` point the same way as `sentence` (the job named 'before' is the one that must finish first)?",
        },
    },
    "hard_vs_soft": {
        "type": "noul",
        "instructions": "Is 'tripled x must be at least 12' a HARD requirement (not a preference)?",
    },
}

# ------------------------------------------------------------- suite F ----


def fanout_battery():
    questions = {}
    for case in SUITE_B:
        intent = case["state"]["intent"]
        questions[f"route_{case['id'][:2].lower()}"] = {
            "type": "choice",
            "instructions": {
                "intent": intent,
                "question": "Which solver rule best matches `intent`?",
            },
            "criteria": RULE_CARDS,
        }
        questions[f"complete_{case['id'][:2].lower()}"] = {
            "type": "noul",
            "instructions": {
                "intent": intent,
                "question": "Is every value required to evaluate the matching rule for `intent` concrete and present?",
            },
        }
    return questions


# ----------------------------------------------------------------- main ----


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="/tmp/jev_poc2")
    parser.add_argument("--suite", default="BCDEF",
                        help="suites to run, e.g. B, BC, E, F, BCDEF")
    args = parser.parse_args()
    os.makedirs(args.out, exist_ok=True)

    if "JEV_API_KEY" not in os.environ:
        print("JEV_API_KEY not set", file=sys.stderr)
        return 2

    report = {"suites": {}}

    # ---- Suite B + C: routing confusion matrix + mode decision ----
    if "B" in args.suite or "C" in args.suite:
        rows = []
        for case in SUITE_B + SUITE_C:
            battery = routing_battery(case.get("extra_questions"))
            try:
                body, latency = call_jev(case["state"], battery)
            except urllib.error.HTTPError as err:
                rows.append({"case": case["id"], "error": f"HTTP {err.code}: {err.read().decode()[:200]}"})
                continue
            answers = body["answers"]
            r = answers["route_rule"]
            weakest = weakest_of(answers, ["route_rule", "solve_mode", "data_complete",
                                           "resource_dimension"])
            row = {
                "case": case["id"],
                "expect": case["expect"],
                "routed": r["choice"], "route_conf": r["confidence"],
                "mode": answers["solve_mode"]["choice"],
                "mode_conf": answers["solve_mode"]["confidence"],
                "data_complete": answers["data_complete"]["noul"],
                "weakest": round(weakest, 3),
                "gate": "open" if weakest >= CONFIDENCE_GATE else "blocked",
                "latency_ms": round(latency, 1),
                "tokens": body.get("usage", {}).get("input_tokens"),
            }
            if "resource_dimension" in answers:
                row["dimension"] = answers["resource_dimension"]["choice"]
            compiled = compile_family(case, answers)
            row["compiled"] = compiled
            with open(os.path.join(args.out, f"{case['id']}.json"), "w") as fh:
                json.dump(compiled, fh, indent=2)
            rows.append(row)
        report["suites"]["BC"] = rows

    # ---- Suite D: generic B-prime depth ----
    if "D" in args.suite:
        rows = []
        for case in D_CASES:
            try:
                body, latency = call_jev(case["state"], case["questions"])
            except urllib.error.HTTPError as err:
                rows.append({"case": case["id"], "error": f"HTTP {err.code}: {err.read().decode()[:200]}"})
                continue
            answers = body["answers"]
            weakest = weakest_of(answers, case["decision_keys"])
            compiled = case["compile"](answers)
            row = {
                "case": case["id"],
                "expect": case["expect"],
                "decisions": {k: (v.get("choice") if v.get("type") != "noul" else v.get("noul"))
                              for k, v in answers.items()},
                "confidences": {k: (v.get("confidence") if v.get("type") != "noul" else v.get("noul"))
                                for k, v in answers.items()},
                "weakest": round(weakest, 3),
                "gate": "open" if weakest >= CONFIDENCE_GATE else "blocked",
                "latency_ms": round(latency, 1),
                "tokens": body.get("usage", {}).get("input_tokens"),
                "compiled": compiled,
            }
            with open(os.path.join(args.out, f"{case['id']}.json"), "w") as fh:
                json.dump(compiled, fh, indent=2)
            rows.append(row)
        report["suites"]["D"] = rows

    # ---- Suite E: stability (5 repeats of the mini-battery) ----
    if "E" in args.suite:
        reps = []
        for i in range(5):
            body, latency = call_jev({"note": "repeat run"}, E_MINI_BATTERY)
            a = body["answers"]
            reps.append({
                "run": i + 1,
                "route": a["route"]["choice"],
                "route_conf": a["route"]["confidence"],
                "same_direction_noul": a["same_direction"]["noul"],
                "hard_noul": a["hard_vs_soft"]["noul"],
                "latency_ms": round(latency, 1),
                "tokens": body.get("usage", {}).get("input_tokens"),
            })
        routes = [r["route"] for r in reps]
        report["suites"]["E"] = {
            "repeats": reps,
            "route_stable": len(set(routes)) == 1,
            "route_spread": {c: routes.count(c) for c in set(routes)},
        }

    # ---- Suite F: fan-out scaling ----
    if "F" in args.suite:
        state = {"note": "batch routing of six intents, one question each"}
        body, latency = call_jev(state, fanout_battery())
        answers = body["answers"]
        per = {}
        for case in SUITE_B:
            key = case["id"][:2].lower()
            per[case["id"]] = {
                "routed": answers[f"route_{key}"]["choice"],
                "conf": answers[f"route_{key}"]["confidence"],
                "complete": answers[f"complete_{key}"]["noul"],
            }
        report["suites"]["F"] = {
            "questions_in_one_call": len(fanout_battery()),
            "latency_ms": round(latency, 1),
            "tokens": body.get("usage"),
            "per_intent": per,
        }

    print(json.dumps(report, indent=2))
    with open(os.path.join(args.out, "report.json"), "w") as fh:
        json.dump(report, fh, indent=2)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
