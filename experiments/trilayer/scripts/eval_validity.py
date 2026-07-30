"""V6: the supersession contract through the real product surfaces.

Requires the validity-plumbed binary (valid_from/valid_until on
openmemory_remember's detailed observations, valid_at on
openmemory_recall).

Treatment per correction pair (old, new), all supported tool calls:
1. read the old doc's gloss (get_entity),
2. forget the open-ended copy,
3. re-remember the SAME gloss content on the old entity with
   valid_from=ORIGIN, valid_until=SUPERSEDED_AT (append-only
   supersede: content preserved, window closed),
4. add the `supersedes` relation new -> old.

Eval grid (current vs history queries):
- graph route, default recall           -> old fact filtered at now
- graph route, valid_at=HISTORY_AT      -> old fact reachable, new
  fact (valid_from = its write time) filtered
- graph route, history WITHOUT pinning  -> the honest cost when no
  temporal intent is detected
- index route raw and with successor promotion (content chunks carry
  no validity; promotion remains the content-layer complement)

The pinned instant is supplied by the harness (oracle instant, same
epistemic position as T7's asof-probe): extraction of the instant
from natural language remains unbuilt and unmeasured.
"""

import json
import os
import statistics
import sys

sys.path.insert(0, os.path.dirname(__file__))
from eval_scenario import Harness, metrics

ORIGIN = 1767225600        # 2026-01-01
SUPERSEDED_AT = 1781740800  # 2026-06-16
HISTORY_AT = 1772323200     # 2026-03-01


def apply_supersession(h, meta):
    for c in meta["corrections"]:
        old, new = c["old"], c["new"]
        payload, _ = h.call("openmemory_get_entity", {"entity": old})
        for obs in payload.get("observations", []):
            content = obs["content"]
            title = obs.get("title") or old
            h.call("openmemory_forget", {"observation_id": obs["id"]})
            h.call("openmemory_remember", {
                "entity": old, "entity_type": "concept",
                "source": "supersession", "memory_tier": "semantic",
                "observations": [{
                    "content": content, "title": title,
                    "concepts": [old.split("/")[0]], "source_files": [old],
                    "valid_from": ORIGIN, "valid_until": SUPERSEDED_AT,
                }]})
        h.call("openmemory_add_relation", {
            "from_entity": new, "from_entity_type": "concept",
            "to_entity": old, "to_entity_type": "concept",
            "relation_type": "supersedes", "source": "supersession"})
        print(f"superseded with validity: {old} (until 2026-06-16)")


def graph_files_at(h, query, valid_at=None):
    args = {"query": query, "limit": 30, "mode": "hybrid"}
    if valid_at is not None:
        args["valid_at"] = valid_at
    p, _ = h.call("openmemory_recall", args)
    files, seen = [], set()
    for r in p.get("results", []):
        n = r.get("entity_name", "")
        if "/" in n and n not in seen:
            seen.add(n)
            files.append(n)
    return files


def promote(files, smap):
    out = []
    for f in files:
        succ = smap.get(f)
        if succ and succ not in out:
            out.append(succ)
        if f not in out:
            out.append(f)
    return out


def agg(rows):
    by = {}
    for cat in sorted({r["category"] for r in rows}):
        sub = [r for r in rows if r["category"] == cat]
        by[cat] = round(statistics.mean(r["mrr"] for r in sub), 4)
    return by


def main():
    scen_dir = os.path.abspath(sys.argv[1])
    meta = json.load(open(os.path.join(scen_dir, "scenario.json")))
    h = Harness(scen_dir)
    h.call("openmemory_search", {"query": "warmup", "limit": 1, "mode": "vector"})
    apply_supersession(h, meta)
    smap = {c["old"]: c["new"] for c in meta["corrections"]}

    queries = [json.loads(l) for l in open(os.path.join(scen_dir, "queries.jsonl"))
               if json.loads(l)["relevant"]]

    conditions = {
        "graph/default": lambda q: graph_files_at(h, q["query"]),
        "graph/pinned-history": lambda q: graph_files_at(
            h, q["query"],
            valid_at=HISTORY_AT if q["category"] == "history" else None),
        "index/raw": lambda q: h.search_files(q["query"], "hybrid")[0],
        "index/promoted": lambda q: promote(h.search_files(q["query"], "hybrid")[0], smap),
    }
    report = {}
    for name, fn in conditions.items():
        rows = []
        for q in queries:
            files = fn(q)
            rows.append({**metrics(files, q["relevant"]), "category": q["category"],
                         "id": q["id"], "top": files[:3]})
        cats = agg(rows)
        report[name] = {"by_category_mrr": cats, "per_query": rows}
        print(f"{name:22s} " + "  ".join(f"{c}={v:.2f}" for c, v in cats.items()))
    h.client.close()
    with open(os.path.join(scen_dir, "eval-validity.json"), "w") as f:
        json.dump(report, f, indent=2)


if __name__ == "__main__":
    main()
