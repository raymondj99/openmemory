# Reich Molecular-Sex Demo

This is the copy-pastable operator guide for the demo implemented by
`scripts/demo-reich/run-demo.sh`.

## Setup

```bash
scripts/demo-reich/run-demo.sh
```

The script rebuilds `target/release/openmemory`, recreates
`/tmp/openmemory-reich-demo`, generates the synthetic `labshare` tree
and BAM corpus, seeds the knowledge graph and free-text index, and
leaves the MCP HTTP server plus watcher running.

The MCP endpoint is:

```text
http://127.0.0.1:7801/mcp
```

## Agent Prompt

Use the prompt in `PROMPT.md` with an MCP client that can call the
`openmemory-demo` tools. Capture the full answer to:

```text
/tmp/openmemory-reich-demo/artifacts/transcript.md
```

Then grade the transcript:

```bash
scripts/demo-reich/run-demo.sh \
  --skip-build \
  --transcript /tmp/openmemory-reich-demo/artifacts/transcript.md
```

For a smoke test of the fixture generation, BAMs, rubric, and TSV diff
without an agent, run:

```bash
scripts/demo-reich/run-demo.sh --reference
```

## Teardown

```bash
kill "$(cat /tmp/openmemory-reich-demo/artifacts/watch.pid)" 2>/dev/null || true
kill "$(cat /tmp/openmemory-reich-demo/artifacts/mcp.pid)" 2>/dev/null || true
```
