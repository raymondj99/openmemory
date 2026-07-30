"""Run all retrieval arms over the authored query set.

Arms:
  index-{keyword,vector,hybrid}   openmemory_search over chunk URIs -> files
  graph-{keyword,vector,hybrid}   openmemory_recall over entities -> files
  tri-{keyword,vector,hybrid}     recall localizes a repo scope, then
                                  uri_prefix-scoped search rank-interleaved
                                  with unscoped search -> files

File-level judgments. Metrics: R@5, R@10, MRR, per-call latency.
Abstention queries are excluded from quality metrics and reported as
score-shape stats (top score, top margin) vs answerable queries.
"""

import json
import os
import statistics
import sys

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
MODES = ["keyword", "vector", "hybrid"]
K_FETCH = 30
K = 10


def uri_to_file(uri):
    if not uri.startswith("omem://"):
        return None
    return uri[len("omem://"):].split("#")[0]


def dedupe_files(items):
    seen, out = set(), []
    for f in items:
        if f and f not in seen:
            seen.add(f)
            out.append(f)
    return out


def search_files(client, query, mode, uri_prefix=None):
    args = {"query": query, "limit": K_FETCH, "mode": mode}
    if uri_prefix:
        args["uri_prefix"] = uri_prefix
    payload, dt = client.call("openmemory_search", args)
    results = payload.get("results", [])
    files = dedupe_files(uri_to_file(r.get("uri", "")) for r in results)
    scores = [r.get("score", 0.0) for r in results]
    return files, scores, dt


def recall_entities(client, query, mode, limit=K_FETCH):
    payload, dt = client.call(
        "openmemory_recall", {"query": query, "limit": limit, "mode": mode})
    return payload.get("results", []), dt


def graph_files(client, query, mode):
    results, dt = recall_entities(client, query, mode)
    files = dedupe_files(
        r["entity_name"] for r in results
        if r.get("entity_type") == "concept" and "/" in r.get("entity_name", ""))
    scores = [r.get("score", 0.0) for r in results]
    return files, scores, dt


def interleave(primary, secondary):
    out, seen = [], set()
    pi, si = iter(primary), iter(secondary)
    pl, sl = list(primary), list(secondary)
    i = 0
    while len(out) < len(pl) + len(sl):
        for lst, idx in ((pl, i), (sl, i)):
            if idx < len(lst) and lst[idx] not in seen:
                seen.add(lst[idx])
                out.append(lst[idx])
        i += 1
        if i > max(len(pl), len(sl)):
            break
    return out


CRATE_PREFIX = {
    "clap_builder": "clap/clap_builder", "clap_derive": "clap/clap_derive",
    "clap_lex": "clap/clap_lex", "clap_complete": "clap/clap_complete",
}
for _c in ["cli", "core", "exec", "tui", "protocol", "config",
           "apply-patch", "arg0", "codex-mcp", "login"]:
    CRATE_PREFIX[f"codex-{_c}"] = f"codex/codex-rs/{_c}"


def scope_prefixes(entity_name):
    """Ancestor URI prefixes an activated entity votes for, shallowest
    to deepest. The graph is one shared project; scope is a concept
    neighborhood (directory subtree), never a hard repo partition."""
    if entity_name in CRATE_PREFIX:
        p = CRATE_PREFIX[entity_name]
        return [p.split("/")[0], p]
    if entity_name in ("codex", "clap", "anyhow"):
        return [entity_name]
    if "/" not in entity_name:
        return []
    parts = entity_name.split("/")[:-1]  # drop the filename
    return ["/".join(parts[:d]) for d in range(1, min(len(parts), 4) + 1)]


def tri_files(client, query, mode):
    """Concept localization -> deepest agreed neighborhood scope ->
    scoped + global rank interleave."""
    ents, dt1 = recall_entities(client, query, mode, limit=10)
    votes = {}
    for r in ents[:5]:
        for p in scope_prefixes(r.get("entity_name", "")):
            votes[p] = votes.get(p, 0) + 1
    scope = None
    agreed = [p for p, v in votes.items() if v >= 3]
    if agreed:
        scope = max(agreed, key=lambda p: p.count("/"))  # deepest wins
    if scope:
        scoped, _, dt2 = search_files(client, query, mode, uri_prefix=f"omem://{scope}/")
        glob, _, dt3 = search_files(client, query, mode)
        return interleave(scoped, glob), [], dt1 + dt2 + dt3, scope
    files, scores, dt2 = search_files(client, query, mode)
    return files, scores, dt1 + dt2, None


def metrics(ranked, relevant):
    rel = set(relevant)
    hits5 = sum(1 for f in ranked[:5] if f in rel)
    hits10 = sum(1 for f in ranked[:10] if f in rel)
    mrr = 0.0
    for i, f in enumerate(ranked[:10]):
        if f in rel:
            mrr = 1.0 / (i + 1)
            break
    return {"r5": hits5 / len(rel), "r10": hits10 / len(rel), "mrr": mrr}


def pctl(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p * len(xs)))] if xs else 0.0


def main():
    queries = [json.loads(l) for l in open(os.path.join(ROOT, "queries/queries.jsonl"))]
    answerable = [q for q in queries if q["relevant"]]
    abstention = [q for q in queries if not q["relevant"]]
    client = McpClient()

    # Warm the vector arm once so model load is not billed to a query.
    import time
    t0 = time.monotonic()
    client.call("openmemory_search", {"query": "warmup", "limit": 1, "mode": "vector"})
    warm = time.monotonic() - t0

    rows, shape_rows = [], []
    arms = [(f"index-{m}", m) for m in MODES] + \
           [(f"graph-{m}", m) for m in MODES] + \
           [(f"tri-{m}", m) for m in MODES]

    for arm, mode in arms:
        per_q, lats, scoped_n = [], [], 0
        for q in queries:
            if arm.startswith("index-"):
                files, scores, dt = search_files(client, q["query"], mode)
                scope = None
            elif arm.startswith("graph-"):
                files, scores, dt = graph_files(client, q["query"], mode)
                scope = None
            else:
                files, scores, dt, scope = tri_files(client, q["query"], mode)
                scoped_n += 1 if scope else 0
            lats.append(dt)
            if q["relevant"]:
                m = metrics(files, q["relevant"])
                per_q.append({**m, "id": q["id"], "category": q["category"],
                              "top": files[:10], "scope": scope})
            if arm.startswith("index-") and scores:
                shape_rows.append({
                    "mode": mode, "answerable": bool(q["relevant"]),
                    "top": scores[0],
                    "margin": scores[0] - scores[1] if len(scores) > 1 else scores[0],
                })
        agg = {
            "arm": arm,
            "R@5": round(statistics.mean(r["r5"] for r in per_q), 4),
            "R@10": round(statistics.mean(r["r10"] for r in per_q), 4),
            "MRR": round(statistics.mean(r["mrr"] for r in per_q), 4),
            "p50_ms": round(1000 * pctl(lats, 0.5), 1),
            "p95_ms": round(1000 * pctl(lats, 0.95), 1),
            "scoped_queries": scoped_n or None,
            "by_category": {},
            "per_query": per_q,
        }
        for cat in sorted({q["category"] for q in answerable}):
            sub = [r for r in per_q if r["category"] == cat]
            agg["by_category"][cat] = {
                "n": len(sub),
                "R@10": round(statistics.mean(r["r10"] for r in sub), 4),
                "MRR": round(statistics.mean(r["mrr"] for r in sub), 4),
            }
        rows.append(agg)
        print(f"{arm:16s} R@5={agg['R@5']:.3f} R@10={agg['R@10']:.3f} "
              f"MRR={agg['MRR']:.3f} p50={agg['p50_ms']}ms p95={agg['p95_ms']}ms")

    shapes = {}
    for mode in MODES:
        for ans in (True, False):
            sel = [s for s in shape_rows if s["mode"] == mode and s["answerable"] == ans]
            if sel:
                shapes[f"{mode}/{'answerable' if ans else 'abstention'}"] = {
                    "n": len(sel),
                    "top_mean": round(statistics.mean(s["top"] for s in sel), 4),
                    "margin_mean": round(statistics.mean(s["margin"] for s in sel), 4),
                }
    client.close()

    out = {"vector_warmup_s": round(warm, 2), "n_queries": len(queries),
           "n_answerable": len(answerable), "n_abstention": len(abstention),
           "arms": rows, "score_shapes": shapes}
    with open(os.path.join(ROOT, "results/eval.json"), "w") as f:
        json.dump(out, f, indent=2)
    print(f"\nvector warmup {warm:.2f}s; full detail in results/eval.json")


if __name__ == "__main__":
    main()
