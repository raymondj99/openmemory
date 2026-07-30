"""Ingest the three pinned repos into one shared openmemory store.

Layer 1: project/crate/file entities + part_of/depends_on/references
relations. Layer 2: one gloss observation per file entity. Layer 3:
file content chunks under omem:// URIs. Emits a manifest of every
ingested file so query authoring judges only against recallable
targets, plus a throughput receipt.
"""

import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient, HOME

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
REPOS = os.path.join(ROOT, "repos")
RESULTS = os.path.join(ROOT, "results")

CHUNK_CHARS = 1800
MAX_CHUNKS_PER_FILE = 6
CODEX_CRATES = [
    "cli", "core", "exec", "tui", "protocol", "config",
    "apply-patch", "arg0", "codex-mcp", "login",
]
CODEX_PER_CRATE_CAP = 50
CODEX_DOCS_CAP = 15
CLAP_CRATES = ["clap_builder", "clap_derive", "clap_lex", "clap_complete"]


def rs_files(base, subdir):
    out = []
    root = os.path.join(base, subdir)
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d != "tests"]
        for f in filenames:
            if f.endswith(".rs"):
                out.append(os.path.join(dirpath, f))
    return sorted(out)


def collect():
    """Return list of (repo, crate, abspath, relpath). crate may be None."""
    files = []
    a = os.path.join(REPOS, "anyhow")
    for p in rs_files(a, "src"):
        files.append(("anyhow", None, p, os.path.relpath(p, a)))
    files.append(("anyhow", None, os.path.join(a, "README.md"), "README.md"))

    c = os.path.join(REPOS, "clap")
    for p in rs_files(c, "src"):
        files.append(("clap", None, p, os.path.relpath(p, c)))
    for crate in CLAP_CRATES:
        if os.path.isdir(os.path.join(c, crate, "src")):
            for p in rs_files(c, os.path.join(crate, "src")):
                files.append(("clap", crate, p, os.path.relpath(p, c)))
    for p in rs_files(c, "examples"):
        files.append(("clap", None, p, os.path.relpath(p, c)))
    for f in sorted(os.listdir(c)):
        if f.endswith(".md"):
            files.append(("clap", None, os.path.join(c, f), f))

    x = os.path.join(REPOS, "codex")
    for crate in CODEX_CRATES:
        crate_dir = os.path.join("codex-rs", crate, "src")
        if not os.path.isdir(os.path.join(x, crate_dir)):
            print(f"warn: codex crate missing: {crate}", file=sys.stderr)
            continue
        fs = rs_files(x, crate_dir)
        fs.sort(key=lambda p: -os.path.getsize(p))
        for p in fs[:CODEX_PER_CRATE_CAP]:
            files.append(("codex", f"codex-{crate}", p, os.path.relpath(p, x)))
    files.append(("codex", None, os.path.join(x, "README.md"), "README.md"))
    docs = [os.path.join(x, "docs", f) for f in sorted(os.listdir(os.path.join(x, "docs")))
            if f.endswith(".md")]
    docs.sort(key=lambda p: -os.path.getsize(p))
    for p in docs[:CODEX_DOCS_CAP]:
        files.append(("codex", None, p, os.path.relpath(p, x)))

    return [f for f in files if os.path.isfile(f[2])]


DOC_RE = re.compile(r"^\s*(//[/!])\s?(.*)")
PUB_RE = re.compile(
    r"^\s*pub(?:\([^)]*\))?\s+"
    r"(?:async\s+)?(?:unsafe\s+)?(fn|struct|enum|trait|mod|type|const|static)\s+"
    r"([A-Za-z0-9_]+)"
)


def gloss(text, relpath):
    """Heuristic file gloss: doc comments + pub item signatures."""
    docs, items = [], []
    for line in text.splitlines():
        m = DOC_RE.match(line)
        if m and len(docs) < 15:
            docs.append(m.group(2).strip())
            continue
        m = PUB_RE.match(line)
        if m and len(items) < 30:
            items.append(f"{m.group(1)} {m.group(2)}")
    parts = [f"File {relpath}."]
    if docs:
        parts.append(" ".join(d for d in docs if d)[:700])
    if items:
        parts.append("Public items: " + ", ".join(items) + ".")
    if relpath.endswith(".md"):
        head = "\n".join(text.splitlines()[:25])
        parts = [f"Document {relpath}.", head[:900]]
    return " ".join(parts)[:1200]


def chunks(text):
    out, buf, size = [], [], 0
    for line in text.splitlines(keepends=True):
        buf.append(line)
        size += len(line)
        if size >= CHUNK_CHARS:
            out.append("".join(buf))
            buf, size = [], 0
    if buf:
        out.append("".join(buf))
    return out[:MAX_CHUNKS_PER_FILE]


USE_EXT = {
    "clap": re.compile(r"^\s*use\s+clap(?:_builder|_derive|_lex|_complete)?::", re.M),
    "anyhow": re.compile(r"^\s*(?:use\s+anyhow::|.*\banyhow::(?:Result|Error|Context|anyhow!|bail!))", re.M),
}


def main():
    os.makedirs(RESULTS, exist_ok=True)
    files = collect()
    print(f"collected {len(files)} files")
    client = McpClient()
    t_start = time.monotonic()
    manifest, timings = [], {"remember": [], "index_text": []}

    for repo, rev in [("codex", "406dc9239"), ("clap", "f8ac8c5f1"), ("anyhow", "1dbe1862a")]:
        client.call("openmemory_remember", {
            "entity": repo, "entity_type": "project", "source": "trilayer-ingest",
            "observations": [f"Pinned repository {repo} at revision {rev}, ingested for the T15 tri-layer retrieval experiment."],
        })
    # Real dependency edges: codex uses both libraries.
    for target in ["clap", "anyhow"]:
        client.call("openmemory_add_relation", {
            "from_entity": "codex", "from_entity_type": "project",
            "to_entity": target, "to_entity_type": "project",
            "relation_type": "depends_on", "source": "trilayer-ingest",
        })

    seen_crates = set()
    n_chunks = 0
    for i, (repo, crate, path, relpath) in enumerate(files):
        try:
            text = open(path, encoding="utf-8", errors="replace").read()
        except OSError as e:
            print(f"skip {relpath}: {e}", file=sys.stderr)
            continue
        entity = f"{repo}/{relpath}"
        uri_base = f"omem://{repo}/{relpath}"

        if crate and crate not in seen_crates:
            seen_crates.add(crate)
            client.call("openmemory_remember", {
                "entity": crate, "entity_type": "concept", "source": "trilayer-ingest",
                "observations": [f"Crate {crate}, a workspace member of the {repo} repository."],
                "relations": [{"to_entity": repo, "to_entity_type": "project",
                               "relation_type": "part_of"}],
            })

        relations = [{"to_entity": crate or repo,
                      "to_entity_type": "concept" if crate else "project",
                      "relation_type": "part_of"}]
        if repo == "codex":
            for ext, rx in USE_EXT.items():
                if rx.search(text):
                    relations.append({"to_entity": ext, "to_entity_type": "project",
                                      "relation_type": "references"})

        _, dt = client.call("openmemory_remember", {
            "entity": entity, "entity_type": "concept", "source": "trilayer-ingest",
            "memory_tier": "semantic",
            "observations": [{
                "content": gloss(text, relpath),
                "title": relpath,
                "concepts": [repo] + ([crate] if crate else []),
                "source_files": [relpath],
            }],
            "relations": relations,
        })
        timings["remember"].append(dt)

        for ci, chunk in enumerate(chunks(text)):
            _, dt = client.call("openmemory_index_text", {
                "uri": f"{uri_base}#{ci}", "text": f"{relpath}\n{chunk}",
            })
            timings["index_text"].append(dt)
            n_chunks += 1

        manifest.append({"repo": repo, "crate": crate, "relpath": relpath,
                         "entity": entity, "uri_base": uri_base})
        if (i + 1) % 100 == 0:
            rate = (i + 1) / (time.monotonic() - t_start)
            print(f"  {i + 1}/{len(files)} files ({rate:.1f} files/s)")

    status, _ = client.call("openmemory_status", {})
    client.close()
    wall = time.monotonic() - t_start

    with open(os.path.join(RESULTS, "manifest.jsonl"), "w") as f:
        for row in manifest:
            f.write(json.dumps(row) + "\n")

    def pctl(xs, p):
        xs = sorted(xs)
        return xs[min(len(xs) - 1, int(p * len(xs)))] if xs else None

    receipt = {
        "files": len(manifest), "chunks": n_chunks, "wall_seconds": round(wall, 1),
        "files_per_sec": round(len(manifest) / wall, 2),
        "remember_p50_ms": round(1000 * pctl(timings["remember"], 0.5), 1),
        "remember_p95_ms": round(1000 * pctl(timings["remember"], 0.95), 1),
        "index_text_p50_ms": round(1000 * pctl(timings["index_text"], 0.5), 1),
        "index_text_p95_ms": round(1000 * pctl(timings["index_text"], 0.95), 1),
        "status": status,
        "home": HOME,
    }
    with open(os.path.join(RESULTS, "ingest-receipt.json"), "w") as f:
        json.dump(receipt, f, indent=2)
    print(json.dumps(receipt, indent=2)[:2000])


if __name__ == "__main__":
    main()
