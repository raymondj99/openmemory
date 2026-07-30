"""Ingest a document-folder scenario into its own isolated store.

Usage: TRILAYER_HOME=<scenario>/.home python3 ingest_docs.py <scenario-dir>

Layer mapping mirrors the code experiment: project entities for the
top-level folders, file entities named <folder>/<relpath> with one
gloss observation (title + first paragraph + heading list), content
chunks under omem:// URIs, `part_of` edges file->project, declared
project relations, and `references` edges mined from cross-folder
path mentions in the text.
"""

import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient

CHUNK_CHARS = 1800
MAX_CHUNKS = 6


def gloss(text, relpath):
    lines = text.splitlines()
    title = next((l[2:].strip() for l in lines if l.startswith("# ")), relpath)
    para, buf = "", []
    for l in lines[1:]:
        if l.startswith("#"):
            if buf:
                break
            continue
        if l.strip():
            buf.append(l.strip())
        elif buf:
            break
    para = " ".join(buf)
    heads = [l.lstrip("# ").strip() for l in lines if l.startswith("##")]
    parts = [f"Document {relpath}.", title + ".", para[:500]]
    if heads:
        parts.append("Sections: " + "; ".join(heads[:10]) + ".")
    return " ".join(parts)[:1100]


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
    return out[:MAX_CHUNKS]


def main():
    scen_dir = os.path.abspath(sys.argv[1])
    meta = json.load(open(os.path.join(scen_dir, "scenario.json")))
    corpus_dir = os.path.join(scen_dir, "corpus")
    files = []
    for dirpath, _, names in os.walk(corpus_dir):
        for n in sorted(names):
            if n.endswith((".md", ".txt")):
                full = os.path.join(dirpath, n)
                files.append((os.path.relpath(full, corpus_dir), full))
    files.sort()
    path_set = {rel for rel, _ in files}
    ref_re = re.compile(r"([a-z0-9_-]+(?:/[a-z0-9._-]+)+\.md)")

    client = McpClient()
    t0 = time.monotonic()
    for proj, desc in meta["projects"].items():
        client.call("openmemory_remember", {
            "entity": proj, "entity_type": "project", "source": "scenario-ingest",
            "observations": [desc]})
    for frm, rel, to in meta["project_relations"]:
        client.call("openmemory_add_relation", {
            "from_entity": frm, "from_entity_type": "project",
            "to_entity": to, "to_entity_type": "project",
            "relation_type": rel, "source": "scenario-ingest"})

    manifest = []
    for relpath, full in files:
        text = open(full).read()
        proj = relpath.split("/")[0]
        relations = [{"to_entity": proj, "to_entity_type": "project",
                      "relation_type": "part_of"}]
        for m in set(ref_re.findall(text)):
            target = m if m in path_set else None
            if target and target != relpath:
                relations.append({"to_entity": target, "to_entity_type": "concept",
                                  "relation_type": "references"})
        for other in meta["projects"]:
            if other != proj and re.search(rf"\b{re.escape(other)}\b", text, re.I):
                relations.append({"to_entity": other, "to_entity_type": "project",
                                  "relation_type": "references"})
        client.call("openmemory_remember", {
            "entity": relpath, "entity_type": "concept", "source": "scenario-ingest",
            "memory_tier": "semantic",
            "observations": [{"content": gloss(text, relpath), "title": relpath,
                              "concepts": [proj], "source_files": [relpath]}],
            "relations": relations})
        for ci, ch in enumerate(chunks(text)):
            client.call("openmemory_index_text",
                        {"uri": f"omem://{relpath}#{ci}", "text": f"{relpath}\n{ch}"})
        manifest.append(relpath)

    status, _ = client.call("openmemory_status", {})
    client.close()
    with open(os.path.join(scen_dir, "manifest.json"), "w") as f:
        json.dump({"files": manifest, "wall_seconds": round(time.monotonic() - t0, 1),
                   "status": status}, f, indent=2)
    print(f"{os.path.basename(scen_dir)}: {len(manifest)} files, "
          f"{status['total_entities']} entities, {status['total_relations']} relations, "
          f"{round(time.monotonic() - t0, 1)}s")


if __name__ == "__main__":
    main()
