"""Generic scenario evaluation: same arms as eval_v2, corpus-agnostic.

Usage: TRILAYER_HOME=<scenario>/.home python3 eval_scenario.py <scenario-dir>

Arms: index-{keyword,vector,hybrid}, graph-hybrid, plan-hybrid,
router-hybrid, router-full. Router rules identical to eval_v2 (fixed
a priori, unchanged for the new corpora on purpose: a query classifier
that only works on Rust identifiers should be exposed, not patched).
"""

import json
import os
import re
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient

K_FETCH = 30
MODES = ["keyword", "vector", "hybrid"]
IDENT_RE = re.compile(r"[a-z0-9]+_[a-z0-9]+|\b[a-z]+[A-Z][A-Za-z]+|!|::")
REL_CUES = ("which ", "what ", "depends", "rely ", "relies", "manages",
            "invoked", "crate that", "workspace", "class ", "course ",
            "project that", "provides", "comes from", "come from")
DENSE_OVERRIDE = 0.05


class Harness:
    def __init__(self, scen_dir):
        self.meta = json.load(open(os.path.join(scen_dir, "scenario.json")))
        self.manifest = set(json.load(open(os.path.join(scen_dir, "manifest.json")))["files"])
        self.projects = set(self.meta["projects"])
        self.client = McpClient()
        self.emap = {}
        offset = 0
        while True:
            p, _ = self.client.call("openmemory_list_entities",
                                    {"limit": 200, "offset": offset})
            ents = p.get("entities", p.get("results", []))
            if not ents:
                break
            for e in ents:
                self.emap[e["id"]] = (e["name"], e["entity_type"])
            offset += len(ents)
        self.rel_cache = {}

    def call(self, tool, args):
        return self.client.call(tool, args)

    def search_files(self, query, mode, uri_prefix=None):
        args = {"query": query, "limit": K_FETCH, "mode": mode}
        if uri_prefix:
            args["uri_prefix"] = uri_prefix
        p, dt = self.call("openmemory_search", args)
        files, seen, scores = [], set(), []
        for r in p.get("results", []):
            scores.append(r.get("score", 0.0))
            u = r.get("uri", "")
            if u.startswith("omem://"):
                f = u[len("omem://"):].split("#")[0]
                if f not in seen:
                    seen.add(f)
                    files.append(f)
        return files, scores, dt

    def recall_entities(self, query, mode, limit=K_FETCH):
        p, dt = self.call("openmemory_recall",
                          {"query": query, "limit": limit, "mode": mode})
        return p.get("results", []), dt

    def graph_files(self, query, mode):
        ents, dt = self.recall_entities(query, mode)
        files, seen = [], set()
        for r in ents:
            n = r.get("entity_name", "")
            if "/" in n and n not in seen:
                seen.add(n)
                files.append(n)
        return files, dt

    def relations_of(self, name):
        if name not in self.rel_cache:
            try:
                p, _ = self.call("openmemory_get_entity", {"entity": name})
                self.rel_cache[name] = (p.get("relations", []),
                                        p.get("entity", {}).get("id"))
            except RuntimeError:
                self.rel_cache[name] = ([], None)
        return self.rel_cache[name]

    def representatives(self, proj):
        return [f"{proj}/{r}" for r in self.meta["representatives"]
                if f"{proj}/{r}" in self.manifest]

    def plan_files(self, query, mode="hybrid"):
        ents, dt = self.recall_entities(query, mode, limit=10)
        t0 = time.monotonic()
        out = [r["entity_name"] for r in ents if "/" in r.get("entity_name", "")]
        neighbors, seen = [], set()
        for r in ents[:4]:
            seed = r.get("entity_name", "")
            if seed in self.projects and seed not in seen:
                seen.add(seed)
                neighbors.append((-1, seed, "project"))
            rels, seed_id = self.relations_of(seed)
            for rel in rels:
                oid = rel["to_entity"] if rel["from_entity"] == seed_id else rel["from_entity"]
                oname, otype = self.emap.get(oid, (None, None))
                if not oname or oname in seen:
                    continue
                seen.add(oname)
                prio = 0 if rel["relation_type"] != "part_of" else 2
                neighbors.append((prio, oname, otype))
        neighbors.sort(key=lambda x: x[0])
        for prio, oname, otype in neighbors[:6]:
            if "/" in oname:
                out.append(oname)
                continue
            if oname in self.projects:
                out.extend(self.representatives(oname))
                scoped, _, _ = self.search_files(query, mode,
                                                 uri_prefix=f"omem://{oname}/")
                out.extend(scoped[:3])
        glob, _, _ = self.search_files(query, mode)
        out.extend(glob)
        seen2, dedup = set(), []
        for f in out:
            if f not in seen2:
                seen2.add(f)
                dedup.append(f)
        return dedup, dt + (time.monotonic() - t0)

    def dense_confidence(self, query):
        _, iscores, dt1 = self.search_files(query, "vector")
        ents, dt2 = self.recall_entities(query, "vector", limit=5)
        v_i = iscores[0] if iscores else 0.0
        v_g = max((r.get("raw_score", r.get("score", 0.0)) for r in ents), default=0.0)
        return v_i, v_g, dt1 + dt2

    def router_files(self, query, full):
        if full and any(c in query.lower() for c in REL_CUES):
            files, dt = self.plan_files(query)
            idx, _, dti = self.search_files(query, "hybrid")
            seen, out = set(), []
            for f in files + idx:
                if f not in seen:
                    seen.add(f)
                    out.append(f)
            return out, dt + dti, "plan"
        primary = "graph" if IDENT_RE.search(query) else "index"
        v_i, v_g, dtc = self.dense_confidence(query)
        if primary == "graph" and v_i - v_g > DENSE_OVERRIDE:
            primary = "index"
        elif primary == "index" and v_g - v_i > DENSE_OVERRIDE:
            primary = "graph"
        g, dtg = self.graph_files(query, "hybrid")
        i, _, dti = self.search_files(query, "hybrid")
        ordered = g + i if primary == "graph" else i + g
        seen, out = set(), []
        for f in ordered:
            if f not in seen:
                seen.add(f)
                out.append(f)
        return out, dtc + dtg + dti, primary


def metrics(ranked, relevant):
    rel = set(relevant)
    m = {"r5": sum(1 for f in ranked[:5] if f in rel) / len(rel),
         "r10": sum(1 for f in ranked[:10] if f in rel) / len(rel), "mrr": 0.0}
    for i, f in enumerate(ranked[:10]):
        if f in rel:
            m["mrr"] = 1.0 / (i + 1)
            break
    return m


def main():
    scen_dir = os.path.abspath(sys.argv[1])
    name = os.path.basename(scen_dir)
    h = Harness(scen_dir)
    queries = [json.loads(l) for l in open(os.path.join(scen_dir, "queries.jsonl"))]
    h.call("openmemory_search", {"query": "warmup", "limit": 1, "mode": "vector"})

    arms = [(f"index-{m}", lambda q, m=m: h.search_files(q, m)[0:3:2]) for m in MODES]
    arms += [("graph-hybrid", lambda q: h.graph_files(q, "hybrid")),
             ("plan-hybrid", lambda q: h.plan_files(q)),
             ("router-hybrid", lambda q: h.router_files(q, False)),
             ("router-full", lambda q: h.router_files(q, True))]

    rows = []
    for arm, fn in arms:
        per_q, routes = [], {}
        for q in queries:
            res = fn(q["query"])
            files, route = res[0], (res[2] if len(res) > 2 else None)
            if route:
                routes[route] = routes.get(route, 0) + 1
            if q["relevant"]:
                per_q.append({**metrics(files, q["relevant"]), "id": q["id"],
                              "category": q["category"], "top": files[:10],
                              "route": route})
        agg = {"arm": arm,
               "R@5": round(statistics.mean(r["r5"] for r in per_q), 4),
               "R@10": round(statistics.mean(r["r10"] for r in per_q), 4),
               "MRR": round(statistics.mean(r["mrr"] for r in per_q), 4),
               "routes": routes or None,
               "by_category": {}, "per_query": per_q}
        for cat in sorted({r["category"] for r in per_q}):
            sub = [r for r in per_q if r["category"] == cat]
            agg["by_category"][cat] = {
                "n": len(sub),
                "R@10": round(statistics.mean(r["r10"] for r in sub), 4),
                "MRR": round(statistics.mean(r["mrr"] for r in sub), 4)}
        rows.append(agg)
        print(f"{name}/{arm:14s} R@5={agg['R@5']:.3f} R@10={agg['R@10']:.3f} "
              f"MRR={agg['MRR']:.3f} {agg['routes'] or ''}")
    h.client.close()
    with open(os.path.join(scen_dir, "eval.json"), "w") as f:
        json.dump({"scenario": name, "arms": rows}, f, indent=2)


if __name__ == "__main__":
    main()
