"""Adversarial stress suite for memory spaces.

Fresh isolated profile per run. Exercises creation, name attacks,
physical isolation across all three layers, layered read-set
validation and determinism, supersession inside a space, and
reopen/persistence — all through the real MCP binary.

Run: TRILAYER_HOME=<dir>/.home python3 scripts/stress_spaces.py <dir>
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient, HOME

REPORT = []
CASES = []


def case(name):
    def deco(fn):
        fn._case_name = name
        CASES.append(fn)
        return fn
    return deco


def record(name, verdict, evidence):
    REPORT.append({"case": name, "verdict": verdict, "evidence": evidence})
    print(f"[{verdict}] {name}: {evidence}"[:160])


def expect_error(c, tool, args):
    try:
        c.call(tool, args)
        return None
    except Exception as e:
        return str(e)


def names_of(payload):
    return [r.get("entity_name", r.get("uri", "?")) for r in payload.get("results", [])]


@case("space create/list round trip")
def create_list(c):
    for name in ["alpha", "beta"]:
        c.call("openmemory_space", {"action": "create", "name": name})
    listed, _ = c.call("openmemory_space", {"action": "list"})
    got = sorted(s["name"] for s in listed["spaces"])
    if got == ["alpha", "beta"] and listed["default"] == "default":
        record("create-list", "PASS", f"spaces={got}, default implicit")
    else:
        record("create-list", "FAIL", f"{listed}")


@case("duplicate create is rejected")
def dup_create(c):
    err = expect_error(c, "openmemory_space", {"action": "create", "name": "alpha"})
    if err and "already exists" in err:
        record("dup-create", "PASS", "second create rejected")
    else:
        record("dup-create", "FAIL", f"err={err}")


@case("hostile space names are all rejected")
def name_attacks(c):
    attacks = ["../escape", "a/b", "a\\b", "Upper", "sp ace", "default",
               "-lead", "trail-", "dou--ble", "x" * 65, ".", "..", ""]
    accepted = []
    for name in attacks:
        if expect_error(c, "openmemory_space", {"action": "create", "name": name}) is None:
            accepted.append(name)
    # No attack directory may exist under spaces/.
    spaces_dir = os.path.join(HOME, "data", "default", "spaces")
    on_disk = sorted(os.listdir(spaces_dir)) if os.path.isdir(spaces_dir) else []
    junk = [d for d in on_disk if d not in ("alpha", "beta", "gamma", "layer1",
                                            "layer2", "layer3", "persist")]
    if not accepted and not junk:
        record("name-attacks", "PASS", f"all {len(attacks)} rejected, disk clean")
    else:
        record("name-attacks", "FAIL", f"accepted={accepted} junk={junk}")


@case("all three layers are physically isolated per space")
def tri_layer_isolation(c):
    # Same entity name, same relation, same URI in both spaces.
    for space, fact in [("alpha", "the alpha-only zorble fact"),
                        ("beta", "the beta-only quux fact")]:
        c.call("openmemory_remember", {
            "entity": "Shared Entity", "observations": [fact], "space": space,
        })
        c.call("openmemory_index_text", {
            "uri": "note://shared", "text": fact, "space": space,
        })
    ra, _ = c.call("openmemory_recall", {"query": "zorble", "mode": "keyword", "space": "alpha"})
    rb, _ = c.call("openmemory_recall", {"query": "zorble", "mode": "keyword", "space": "beta"})
    sa, _ = c.call("openmemory_search", {"query": "quux", "mode": "keyword", "space": "alpha"})
    sb, _ = c.call("openmemory_search", {"query": "quux", "mode": "keyword", "space": "beta"})
    rd, _ = c.call("openmemory_recall", {"query": "zorble", "mode": "keyword"})
    ok = (len(ra["results"]) == 1 and not rb["results"]
          and not sa["results"] and len(sb["results"]) == 1
          and not rd["results"])
    if ok:
        record("tri-layer-isolation", "PASS",
               "graph+index rows visible only in their own space; default clean")
    else:
        record("tri-layer-isolation", "FAIL",
               f"ra={len(ra['results'])} rb={len(rb['results'])} "
               f"sa={len(sa['results'])} sb={len(sb['results'])} rd={len(rd['results'])}")


@case("relations never span spaces")
def relation_isolation(c):
    c.call("openmemory_remember", {
        "entity": "only-in-alpha", "observations": ["exists in alpha"], "space": "alpha"})
    err = expect_error(c, "openmemory_add_relation", {
        "from_entity": "Shared Entity", "to_entity": "only-in-alpha",
        "relation_type": "references", "space": "beta",
    })
    if err and "not found" in err:
        record("relation-isolation", "PASS", "cross-space edge rejected: to_entity not found in beta")
    else:
        record("relation-isolation", "FAIL", f"err={err}")


@case("read_spaces validation: bound, dups, unknown, exclusivity")
def read_set_validation(c):
    checks = [
        ({"query": "q", "read_spaces": ["alpha", "beta", "a3", "a4", "a5"]}, "at most"),
        ({"query": "q", "read_spaces": ["alpha", "alpha"]}, "more than once"),
        ({"query": "q", "read_spaces": ["ghost"]}, "does not exist"),
        ({"query": "q", "read_spaces": []}, "at least one"),
        ({"query": "q", "space": "alpha", "read_spaces": ["beta"]}, "mutually exclusive"),
    ]
    bad = []
    for args, needle in checks:
        err = expect_error(c, "openmemory_recall", args)
        if not err or needle not in err:
            bad.append((args, err))
    if not bad:
        record("read-set-validation", "PASS", f"{len(checks)} malformed read sets rejected")
    else:
        record("read-set-validation", "FAIL", f"{bad[0]}")


@case("layered reads interleave by rank in read-set order and stay deterministic")
def layered_determinism(c):
    payloads = []
    for _ in range(4):
        p, _ = c.call("openmemory_recall", {
            "query": "fact", "mode": "keyword",
            "read_spaces": ["alpha", "beta"],
        })
        payloads.append(json.dumps(p, sort_keys=True))
    p = json.loads(payloads[0])
    spaces_in_order = [r["space"] for r in p["results"][:2]]
    if len(set(payloads)) == 1 and spaces_in_order == ["alpha", "beta"]:
        record("layered-determinism", "PASS",
               f"4/4 identical, rank-0 order={spaces_in_order}, fusion={p['fusion']}")
    else:
        record("layered-determinism", "FAIL",
               f"distinct={len(set(payloads))} order={spaces_in_order}")


@case("supersession and as_of work inside a space")
def supersede_in_space(c):
    c.call("openmemory_space", {"action": "create", "name": "gamma"})
    c.call("openmemory_remember", {
        "entity": "gamma-db-choice", "space": "gamma",
        "observations": [{"content": "the database is MySQL", "valid_from": 1000}],
    })
    c.call("openmemory_supersede", {
        "old_entity": "gamma-db-choice", "new_entity": "gamma-db-choice-v2",
        "new_content": "the database is Postgres", "superseded_at": 2000,
        "space": "gamma",
    })
    cur, _ = c.call("openmemory_recall", {
        "query": "database", "mode": "keyword", "space": "gamma"})
    cur_txt = json.dumps(cur)
    old, _ = c.call("openmemory_recall", {
        "query": "database", "mode": "keyword", "space": "gamma", "valid_at": 1500})
    old_txt = json.dumps(old)
    other, _ = c.call("openmemory_recall", {"query": "database", "mode": "keyword", "space": "alpha"})
    ok = ("Postgres" in cur_txt and "MySQL" not in cur_txt
          and "MySQL" in old_txt and not other["results"])
    if ok:
        record("supersede-in-space", "PASS",
               "current=Postgres only, as-of-1500=MySQL, invisible from alpha")
    else:
        record("supersede-in-space", "FAIL", f"cur={cur_txt[:80]} old={old_txt[:80]}")


@case("layered retrieve runs the routed pipeline per space with traces")
def layered_retrieve(c):
    p, _ = c.call("openmemory_retrieve", {
        "query": "database", "read_spaces": ["gamma", "alpha"], "engage": True,
    })
    traces = p["trace"]["spaces"]
    labels = [t["space"] for t in traces]
    rows_labeled = all("space" in r for r in p["results"])
    if (p["trace"]["fusion"] == "rank_interleave" and labels == ["gamma", "alpha"]
            and rows_labeled and p["results"]):
        record("layered-retrieve", "PASS",
               f"per-space traces={labels}, {len(p['results'])} labeled rows")
    else:
        record("layered-retrieve", "FAIL", f"{p['trace']}")


@case("spaces survive server restart with identity intact")
def persistence(c):
    c.call("openmemory_space", {"action": "create", "name": "persist"})
    c.call("openmemory_remember", {
        "entity": "durable-fact", "observations": ["survives restart"], "space": "persist"})
    listed, _ = c.call("openmemory_space", {"action": "list"})
    ids = {s["name"]: s["space_id"] for s in listed["spaces"]}
    c2 = McpClient()
    try:
        listed2, _ = c2.call("openmemory_space", {"action": "list"})
        ids2 = {s["name"]: s["space_id"] for s in listed2["spaces"]}
        r, _ = c2.call("openmemory_recall", {
            "query": "survives restart", "mode": "keyword", "space": "persist"})
        if ids == ids2 and r["results"]:
            record("persistence", "PASS",
                   f"{len(ids)} spaces stable across restart, fact recallable")
        else:
            record("persistence", "FAIL", f"ids match={ids == ids2} hits={len(r['results'])}")
    finally:
        c2.close()


def main():
    scen_dir = os.path.abspath(sys.argv[1])
    os.makedirs(scen_dir, exist_ok=True)
    c = McpClient()
    c.call("openmemory_search", {"query": "warmup", "limit": 1, "mode": "vector"})
    for fn in CASES:
        try:
            fn(c)
        except Exception as e:
            record(fn._case_name.split(":")[0], "FAIL", f"exception: {e!r}"[:150])
    c.close()
    with open(os.path.join(scen_dir, "stress-spaces-report.json"), "w") as f:
        json.dump(REPORT, f, indent=2)
    counts = {}
    for r in REPORT:
        counts[r["verdict"]] = counts.get(r["verdict"], 0) + 1
    print(f"\nTOTAL: {counts}")


if __name__ == "__main__":
    main()
