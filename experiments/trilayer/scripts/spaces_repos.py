"""Memory-spaces validation on the codex/clap/anyhow corpus.

Ingests each pinned repo into its OWN memory space (same file set,
glosses, chunks, and in-repo relations as ingest.py), then evaluates
the 55-query set two ways:

  spaces-layered   openmemory_retrieve with read_spaces
                   ["codex","clap","anyhow"]: the routed tri-layer
                   pipeline runs per space, final lists fused by rank
                   interleaving.
  spaces-routed    the query's repo is known to the caller (the agent
                   usually knows which project it is working in), so
                   retrieve targets that single space.

Compare against results/eval2.json (one shared store). Also checks
isolation: a codex-only identifier must return nothing from the clap
space.

Run: TRILAYER_HOME=<dir>/.home python3 scripts/spaces_repos.py <dir>
"""

import json
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient
from ingest import collect, gloss, chunks
from run_eval import ROOT, metrics, pctl, uri_to_file, dedupe_files, CRATE_PREFIX

REPOS = ("codex", "clap", "anyhow")


def ingest_spaces(client):
    for repo in REPOS:
        client.call("openmemory_space", {"action": "create", "name": repo})
    for repo, rev in [("codex", "406dc9239"), ("clap", "f8ac8c5f1"),
                      ("anyhow", "1dbe1862a")]:
        client.call("openmemory_remember", {
            "entity": repo, "entity_type": "project", "source": "spaces-ingest",
            "space": repo,
            "observations": [f"Pinned repository {repo} at revision {rev}."],
        })
    # codex's knowledge of its dependencies lives inside the codex
    # space as stub project entities (relations never span spaces).
    for dep in ["clap", "anyhow"]:
        client.call("openmemory_remember", {
            "entity": dep, "entity_type": "project", "source": "spaces-ingest",
            "space": "codex",
            "observations": [f"External dependency {dep}, ingested in its own space."],
        })
        client.call("openmemory_add_relation", {
            "from_entity": "codex", "from_entity_type": "project",
            "to_entity": dep, "to_entity_type": "project",
            "relation_type": "depends_on", "source": "spaces-ingest",
            "space": "codex",
        })

    files = collect()
    seen_crates, n_files, n_chunks = set(), 0, 0
    t0 = time.monotonic()
    for repo, crate, path, relpath in files:
        try:
            text = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        entity = f"{repo}/{relpath}"
        if crate and crate not in seen_crates:
            seen_crates.add(crate)
            client.call("openmemory_remember", {
                "entity": crate, "entity_type": "concept", "source": "spaces-ingest",
                "space": repo,
                "observations": [f"Crate {crate}, a workspace member of {repo}."],
                "relations": [{"to_entity": repo, "to_entity_type": "project",
                               "relation_type": "part_of"}],
            })
        client.call("openmemory_remember", {
            "entity": entity, "entity_type": "concept", "source": "spaces-ingest",
            "memory_tier": "semantic", "space": repo,
            "observations": [{
                "content": gloss(text, relpath),
                "title": relpath,
                "concepts": [repo] + ([crate] if crate else []),
                "source_files": [relpath],
            }],
            "relations": [{"to_entity": crate or repo,
                           "to_entity_type": "concept" if crate else "project",
                           "relation_type": "part_of"}],
        })
        n_files += 1
        for ci, chunk in enumerate(chunks(text)):
            client.call("openmemory_index_text", {
                "uri": f"omem://{repo}/{relpath}#{ci}",
                "text": f"{relpath}\n{chunk}",
                "space": repo,
            })
            n_chunks += 1
    dt = time.monotonic() - t0
    print(f"ingested {n_files} files / {n_chunks} chunks into 3 spaces in {dt:.1f}s")


def result_files(payload):
    """Map retrieve rows to file keys: URIs directly; graph entities via
    their path-shaped names or crate prefixes."""
    out = []
    for r in payload.get("results", []):
        uri = r.get("uri")
        if uri:
            out.append(uri_to_file(uri))
            continue
        name = r.get("entity_name", "")
        if "/" in name:
            out.append(name)
        elif name in CRATE_PREFIX:
            out.append(None)  # crate entity: not a file answer
    return dedupe_files(out)


def evaluate(client, queries):
    arms = {}

    def run(arm, fn):
        per_q, lats = [], []
        for q in queries:
            t0 = time.monotonic()
            files = fn(q)
            lats.append(time.monotonic() - t0)
            if q["relevant"]:
                per_q.append({**metrics(files, q["relevant"]),
                              "id": q["id"], "category": q["category"],
                              "top": files[:10]})
        agg = {"arm": arm,
               "R@5": round(statistics.mean(r["r5"] for r in per_q), 4),
               "R@10": round(statistics.mean(r["r10"] for r in per_q), 4),
               "MRR": round(statistics.mean(r["mrr"] for r in per_q), 4),
               "p50_ms": round(1000 * pctl(lats, 0.5), 1),
               "p95_ms": round(1000 * pctl(lats, 0.95), 1),
               "per_query": per_q}
        print(f"{arm:16s} R@5={agg['R@5']:.3f} R@10={agg['R@10']:.3f} "
              f"MRR={agg['MRR']:.3f} p50={agg['p50_ms']}ms p95={agg['p95_ms']}ms")
        arms[arm] = agg
        return agg

    def layered(q):
        payload, _ = client.call("openmemory_retrieve", {
            "query": q["query"], "limit": 30,
            "read_spaces": list(REPOS), "engage": True,
        })
        return result_files(payload)

    def routed(q):
        # The caller knows the repo (first path component of the first
        # relevant file). Abstention queries have no repo; fall back to
        # the layered read.
        if not q["relevant"]:
            return layered(q)
        repo = q["relevant"][0].split("/")[0]
        payload, _ = client.call("openmemory_retrieve", {
            "query": q["query"], "limit": 30, "space": repo, "engage": True,
        })
        return result_files(payload)

    run("spaces-layered", layered)
    run("spaces-routed", routed)
    return arms


def isolation_check(client):
    """A codex-only identifier from the clap space must return nothing
    codex-related."""
    payload, _ = client.call("openmemory_retrieve", {
        "query": "seek_sequence", "space": "clap", "engage": True, "limit": 10})
    leaked = [f for f in result_files(payload) if f and f.startswith("codex/")]
    ok = not leaked
    print(f"isolation: codex identifier from clap space leaked={leaked or 'nothing'}")
    return ok


def main():
    scen_dir = os.path.abspath(sys.argv[1])
    os.makedirs(scen_dir, exist_ok=True)
    queries = [json.loads(l) for l in
               open(os.path.join(ROOT, "queries/queries.jsonl"))]
    client = McpClient()
    client.call("openmemory_search", {"query": "warmup", "limit": 1, "mode": "vector"})
    ingest_spaces(client)
    arms = evaluate(client, queries)
    isolated = isolation_check(client)
    client.close()
    with open(os.path.join(scen_dir, "spaces-repos-report.json"), "w") as f:
        json.dump({"n_queries": len(queries), "isolated": isolated,
                   "arms": arms}, f, indent=2)
    print("report written")


if __name__ == "__main__":
    main()
