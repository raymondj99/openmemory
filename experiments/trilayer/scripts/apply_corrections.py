"""Apply the teamwiki corrections through today's product surfaces.

For each (old, new) pair declared in scenario.json:
1. forget the stale doc's gloss observation (soft delete),
2. remember an OUTDATED marker on the stale entity with
   source="correction" (which recall boosts 1.3x by design),
3. add a `supersedes` relation new -> old.

This is exactly what an MCP agent could do today. The index layer is
deliberately left untouched (the stale file still exists on disk in
real life), so the before/after eval isolates what correction changes
per route.
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from mcp_client import McpClient


def main():
    scen_dir = os.path.abspath(sys.argv[1])
    meta = json.load(open(os.path.join(scen_dir, "scenario.json")))
    client = McpClient()
    for c in meta["corrections"]:
        old, new = c["old"], c["new"]
        payload, _ = client.call("openmemory_get_entity", {"entity": old})
        for obs in payload.get("observations", []):
            client.call("openmemory_forget", {"observation_id": obs["id"]})
        client.call("openmemory_remember", {
            "entity": old, "entity_type": "concept", "source": "correction",
            "observations": [{
                "content": f"OUTDATED: {old} was superseded. The current version is {new}.",
                "title": old}]})
        client.call("openmemory_add_relation", {
            "from_entity": new, "from_entity_type": "concept",
            "to_entity": old, "to_entity_type": "concept",
            "relation_type": "supersedes", "source": "correction"})
        print(f"corrected: {old} -> superseded by {new}")
    client.close()


if __name__ == "__main__":
    main()
