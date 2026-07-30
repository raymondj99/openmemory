"""Supersession prototype: the plan/17 correction contract, harness-side.

Store treatment (vs V4's correction-as-forget): NOTHING is forgotten
and no marker observations are written. The only mutation is the
`supersedes` relation new -> old, added through the supported
add_relation surface (done here inline before the eval).

Retrieval treatment (successor promotion): the superseded map is read
back FROM THE GRAPH (get_entity relations, not from scenario.json),
and any ranked file that has a successor yields its rank to that
successor, dropping to the slot directly after it. Within-fact
rerouting: deterministic, edge-driven, no score touched, history
demoted exactly one position instead of destroyed.

Variant B additionally bypasses promotion when the query carries
explicit history intent (a narrow a priori cue list): a stand-in for
the plan/17 valid_at pinning that the MCP surface does not yet expose.

Conditions reported per arm: raw (no promotion), promoted (A),
promoted+history-bypass (B).
"""

import json
import os
import re
import statistics
import sys

sys.path.insert(0, os.path.dirname(__file__))
from eval_scenario import Harness, metrics

HIST_CUES = ("originally", "old ", "before the", "previous", "used to",
             "first quarter", "at first", " history")


def build_superseded_map(h):
    """old_file -> new_file, read from the graph's supersedes edges."""
    out = {}
    for name in list(h.emap.values()):
        pass
    for eid, (name, etype) in h.emap.items():
        if "/" not in name:
            continue
        rels, my_id = h.relations_of(name)
        for r in rels:
            if r["relation_type"] != "supersedes":
                continue
            frm = h.emap.get(r["from_entity"], (None, None))[0]
            to = h.emap.get(r["to_entity"], (None, None))[0]
            if frm and to:
                out[to] = frm
    return out


def promote(files, smap):
    out = []
    for f in files:
        succ = smap.get(f)
        if succ and succ not in out:
            out.append(succ)
        if f not in out:
            out.append(f)
    return out


def main():
    scen_dir = os.path.abspath(sys.argv[1])
    meta = json.load(open(os.path.join(scen_dir, "scenario.json")))
    h = Harness(scen_dir)
    h.call("openmemory_search", {"query": "warmup", "limit": 1, "mode": "vector"})

    # The one store mutation: supersedes edges through the product surface.
    for c in meta.get("corrections", []):
        try:
            h.call("openmemory_add_relation", {
                "from_entity": c["new"], "from_entity_type": "concept",
                "to_entity": c["old"], "to_entity_type": "concept",
                "relation_type": "supersedes", "source": "correction"})
        except RuntimeError as e:
            if "exists" not in str(e).lower():
                raise
    h.rel_cache.clear()
    smap = build_superseded_map(h)
    print(f"superseded map from graph: {smap}")

    queries = [json.loads(l) for l in open(os.path.join(scen_dir, "queries.jsonl"))]
    arms = {
        "index-hybrid": lambda q: h.search_files(q, "hybrid")[0],
        "graph-hybrid": lambda q: h.graph_files(q, "hybrid")[0],
        "plan-hybrid": lambda q: h.plan_files(q)[0],
        "router-full": lambda q: h.router_files(q, True)[0],
    }
    report = {}
    for arm, fn in arms.items():
        rows = {"raw": [], "promoted": [], "hist-bypass": []}
        for q in queries:
            if not q["relevant"]:
                continue
            files = fn(q["query"])
            hist = any(c in q["query"].lower() for c in HIST_CUES)
            variants = {
                "raw": files,
                "promoted": promote(files, smap),
                "hist-bypass": files if hist else promote(files, smap),
            }
            for k, v in variants.items():
                rows[k].append({**metrics(v, q["relevant"]),
                                "id": q["id"], "category": q["category"],
                                "top": v[:5]})
        report[arm] = {}
        for k, per_q in rows.items():
            agg = {"MRR": round(statistics.mean(r["mrr"] for r in per_q), 4),
                   "R@10": round(statistics.mean(r["r10"] for r in per_q), 4),
                   "by_category": {}}
            for cat in sorted({r["category"] for r in per_q}):
                sub = [r for r in per_q if r["category"] == cat]
                agg["by_category"][cat] = {
                    "MRR": round(statistics.mean(r["mrr"] for r in sub), 4),
                    "R@10": round(statistics.mean(r["r10"] for r in sub), 4)}
            agg["per_query"] = per_q
            report[arm][k] = agg
            cur = agg["by_category"].get("current", {})
            his = agg["by_category"].get("history", {})
            print(f"{arm:14s} {k:12s} MRR={agg['MRR']:.3f} "
                  f"current={cur.get('MRR', 0):.2f} history={his.get('MRR', 0):.2f}")
    h.client.close()
    with open(os.path.join(scen_dir, "eval-supersession.json"), "w") as f:
        json.dump(report, f, indent=2)


if __name__ == "__main__":
    main()
