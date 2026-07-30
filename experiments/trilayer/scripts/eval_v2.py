"""V2 evaluation: everything in run_eval.py plus three new arms.

  plan-hybrid    typed traversal planner: recall seeds -> walk
                 references/depends_on/part_of edges -> entity answers
                 map to representative files + neighborhood-scoped
                 search -> global fill. Deterministic, no score fusion.
  router-hybrid  calibrated router between graph-hybrid and
                 index-hybrid: query-shape rule picks the primary,
                 dense-score margin can override, secondary appended
                 after primary (fill, never interleave).
  router-full    router that additionally dispatches relational-cue
                 queries to the planner.

Router rules are fixed a priori and documented here, not fitted to the
judgments: identifier-shaped means snake_case, CamelCase, `!`, or `::`
in the query; relational cues are the words listed in REL_CUES; the
dense override threshold is 0.05 cosine, chosen before any run.
All 12 arms are re-run fresh so the receipt is one consistent pass.
"""

import json
import os
import re
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient
from run_eval import (ROOT, MODES, K_FETCH, dedupe_files, search_files,
                      recall_entities, graph_files, tri_files, interleave,
                      metrics, pctl, CRATE_PREFIX)

IDENT_RE = re.compile(r"[a-z0-9]+_[a-z0-9]+|\b[a-z]+[A-Z][A-Za-z]+|!|::")
REL_CUES = ("which ", "what ", "depends", "rely ", "relies", "manages",
            "invoked", "crate that", "workspace")
DENSE_OVERRIDE = 0.05

_entity_map = None       # id -> (name, type)
_relations_cache = {}    # entity name -> relations list


def entity_map(client):
    global _entity_map
    if _entity_map is None:
        _entity_map = {}
        offset = 0
        while True:
            payload, _ = client.call("openmemory_list_entities",
                                     {"limit": 200, "offset": offset})
            ents = payload.get("entities", payload.get("results", []))
            if not ents:
                break
            for e in ents:
                _entity_map[e["id"]] = (e["name"], e["entity_type"])
            offset += len(ents)
    return _entity_map


def relations_of(client, name):
    if name not in _relations_cache:
        try:
            payload, _ = client.call("openmemory_get_entity", {"entity": name})
            _relations_cache[name] = (payload.get("relations", []),
                                      payload.get("entity", {}).get("id"))
        except RuntimeError:
            _relations_cache[name] = ([], None)
    return _relations_cache[name]


MANIFEST = {row["entity"] for row in
            (json.loads(l) for l in open(os.path.join(ROOT, "results/manifest.jsonl")))}


def entity_prefix(name, etype):
    """URI-path prefix a graph entity denotes, or None."""
    if name in CRATE_PREFIX:
        return CRATE_PREFIX[name]
    if etype == "project" or name in ("codex", "clap", "anyhow"):
        return name
    return None


def representative_files(prefix):
    """Deterministic convention: an entity-level answer is proxied at
    file level by its defining root files."""
    cands = [f"{prefix}/src/lib.rs", f"{prefix}/README.md"]
    return [c for c in cands if c in MANIFEST]


def plan_files(client, query, mode="hybrid"):
    """Typed traversal planner."""
    ents, dt = recall_entities(client, query, mode, limit=10)
    t0 = time.monotonic()
    out = []
    # file-entity seeds first, in recall order
    for r in ents:
        n = r.get("entity_name", "")
        if "/" in n:
            out.append(n)
    # collect typed neighbors of the top seeds
    emap = entity_map(client)
    prio_of = {"references": 0, "depends_on": 0, "part_of": 2}
    neighbors, seen_n = [], set()
    for r in ents[:4]:
        seed = r.get("entity_name", "")
        seed_type = r.get("entity_type", "")
        # a crate/project seed is its own best neighborhood
        p = entity_prefix(seed, seed_type)
        if p and seed not in seen_n:
            seen_n.add(seed)
            neighbors.append((-1, seed, seed_type))
        rels, seed_id = relations_of(client, seed)
        for rel in rels:
            other_id = rel["to_entity"] if rel["from_entity"] == seed_id else rel["from_entity"]
            oname, otype = emap.get(other_id, (None, None))
            if not oname or oname in seen_n:
                continue
            seen_n.add(oname)
            neighbors.append((prio_of.get(rel["relation_type"], 3), oname, otype))
    neighbors.sort(key=lambda x: x[0])
    for prio, oname, otype in neighbors[:6]:
        if "/" in oname:                      # a file entity neighbor
            out.append(oname)
            continue
        prefix = entity_prefix(oname, otype)
        if not prefix:
            continue
        out.extend(representative_files(prefix))
        scoped, _, dts = search_files(client, query, mode,
                                      uri_prefix=f"omem://{prefix}/")
        out.extend(scoped[:3])
    # global fill
    glob, _, dtg = search_files(client, query, mode)
    out.extend(glob)
    return dedupe_files(out), dt + dtg + (time.monotonic() - t0)


def dense_confidence(client, query):
    """Top dense scores for both routes: the calibration signal."""
    _, iscores, dt1 = search_files(client, query, "vector")
    ents, dt2 = recall_entities(client, query, "vector", limit=5)
    v_index = iscores[0] if iscores else 0.0
    v_graph = max((r.get("raw_score", r.get("score", 0.0)) for r in ents), default=0.0)
    return v_index, v_graph, dt1 + dt2


def router_files(client, query, full=False):
    dt_total = 0.0
    if full and any(c in query.lower() for c in REL_CUES):
        files, dt = plan_files(client, query)
        idx, _, dti = search_files(client, query, "hybrid")
        return dedupe_files(files + idx), dt + dti, "plan"
    primary = "graph" if IDENT_RE.search(query) else "index"
    v_index, v_graph, dtc = dense_confidence(client, query)
    dt_total += dtc
    if primary == "graph" and v_index - v_graph > DENSE_OVERRIDE:
        primary = "index"
    elif primary == "index" and v_graph - v_index > DENSE_OVERRIDE:
        primary = "graph"
    gfiles, _, dtg = graph_files(client, query, "hybrid")
    ifiles, _, dti = search_files(client, query, "hybrid")
    dt_total += dtg + dti
    ordered = gfiles + ifiles if primary == "graph" else ifiles + gfiles
    return dedupe_files(ordered), dt_total, primary


def main():
    queries = [json.loads(l) for l in open(os.path.join(ROOT, "queries/queries.jsonl"))]
    answerable = [q for q in queries if q["relevant"]]
    client = McpClient()
    client.call("openmemory_search", {"query": "warmup", "limit": 1, "mode": "vector"})
    entity_map(client)

    def run_arm(arm, fn):
        per_q, lats = [], []
        route_counts = {}
        for q in queries:
            res = fn(q)
            files, dt = res[0], res[1]
            route = res[2] if len(res) > 2 else None
            lats.append(dt)
            if route:
                route_counts[route] = route_counts.get(route, 0) + 1
            if q["relevant"]:
                m = metrics(files, q["relevant"])
                per_q.append({**m, "id": q["id"], "category": q["category"],
                              "route": route, "top": files[:10]})
        agg = {"arm": arm,
               "R@5": round(statistics.mean(r["r5"] for r in per_q), 4),
               "R@10": round(statistics.mean(r["r10"] for r in per_q), 4),
               "MRR": round(statistics.mean(r["mrr"] for r in per_q), 4),
               "p50_ms": round(1000 * pctl(lats, 0.5), 1),
               "p95_ms": round(1000 * pctl(lats, 0.95), 1),
               "routes": route_counts or None, "by_category": {}, "per_query": per_q}
        for cat in sorted({q["category"] for q in answerable}):
            sub = [r for r in per_q if r["category"] == cat]
            agg["by_category"][cat] = {
                "n": len(sub),
                "R@10": round(statistics.mean(r["r10"] for r in sub), 4),
                "MRR": round(statistics.mean(r["mrr"] for r in sub), 4)}
        print(f"{arm:15s} R@5={agg['R@5']:.3f} R@10={agg['R@10']:.3f} "
              f"MRR={agg['MRR']:.3f} p50={agg['p50_ms']}ms p95={agg['p95_ms']}ms "
              f"{agg['routes'] or ''}")
        return agg

    rows = []
    for m in MODES:
        rows.append(run_arm(f"index-{m}", lambda q, m=m: search_files(client, q["query"], m)[0::2]))
    for m in MODES:
        rows.append(run_arm(f"graph-{m}", lambda q, m=m: graph_files(client, q["query"], m)[0::2]))
    for m in MODES:
        rows.append(run_arm(f"tri-{m}", lambda q, m=m: tri_files(client, q["query"], m)[0:3:2]))
    rows.append(run_arm("plan-hybrid", lambda q: plan_files(client, q["query"])))
    rows.append(run_arm("router-hybrid", lambda q: router_files(client, q["query"], full=False)))
    rows.append(run_arm("router-full", lambda q: router_files(client, q["query"], full=True)))
    client.close()

    with open(os.path.join(ROOT, "results/eval2.json"), "w") as f:
        json.dump({"n_queries": len(queries), "arms": rows}, f, indent=2)
    print("\nfull detail in results/eval2.json")


if __name__ == "__main__":
    main()
