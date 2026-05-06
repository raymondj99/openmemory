# Contributing to openmemory

Thanks for considering a contribution. The repo is small enough that
this file fits in one screen — read it before opening a PR.

## Local development loop

```bash
cargo fmt --all
cargo build --workspace --locked
cargo test --workspace --all-features
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo deny check
```

CI mirrors all of the above plus `--no-default-features` variants of
test, clippy, and the default-features doc gate. Get them green
locally before pushing — `--no-default-features` in particular has
caught feature-gated import bugs that the default-features matrix
misses.

MSRV is **1.85.0** (pinned via `rust-toolchain.toml`). `rmcp` and
several model-runtime crates require 1.88+; the workspace
deliberately ships hand-rolled equivalents in
`openmemory-mcp::protocol` and pins `ort` / `ort-sys` to
`2.0.0-rc.9` to stay under that bar. If a new dependency needs
1.88+, find an older version that doesn't.

## Commit hygiene

Conventional-commit prefixes (`feat`, `fix`, `docs`, `test`, `chore`,
`ci`, `style`, `refactor`). Keep commits surgical — one concern per
commit — and write commit bodies that explain *why* not *what*. PRs
should be reviewable as a sequence of those commits, not a single
squashed dump.

## Test from a hosted instance (GitHub Codespaces + claude.ai)

When you need to validate the Streamable-HTTP transport against a
real MCP client without installing anything on your laptop, the
`.devcontainer/devcontainer.json` plus a free GitHub Codespace plus
a claude.ai custom connector is the cheapest path.

1. **Open a codespace.** From the GitHub repo page → `Code` →
   `Codespaces` → `Create codespace on main`. The devcontainer
   pre-fetches the cargo registry; first launch is ~30 seconds.

2. **Build with the HTTP transport feature.** Inside the codespace
   terminal:

   ```bash
   cargo build --release --features mcp-http -p openmemory-cli
   ```

   ~2 minutes on the default 4 vCPU / 16 GB codespace. Drop
   `--release` for a faster debug build if you only want to
   smoke-test the wire format.

3. **Generate a token and start the server.**

   ```bash
   export OPENMEMORY_HTTP_TOKEN="$(openssl rand -hex 32)"
   echo "Token: $OPENMEMORY_HTTP_TOKEN"   # save this — claude.ai needs it
   ./target/release/openmemory init
   ./target/release/openmemory mcp --http 0.0.0.0:7800
   ```

4. **Make port 7800 public.** In the VS Code "Ports" panel,
   right-click the forwarded `7800` row and set
   `Port Visibility → Public`. Copy the resulting
   `https://<codespace>-7800.app.github.dev` URL.

5. **Register as a connector in claude.ai.** Settings →
   *Connectors* → *Add custom MCP server*:

   - **URL:** `https://<codespace>-7800.app.github.dev/mcp`
   - **Authentication:** Custom header
     `Authorization: Bearer <token>`

   Save. claude.ai handshakes (`initialize` + `tools/list`); you
   should see two log lines on the codespace terminal almost
   immediately.

6. **Smoke test.** From a claude.ai conversation, ask the model to
   "remember that I prefer Rust" and then "what do you remember
   about my language preferences?" The connector should call
   `openmemory_remember` followed by `openmemory_recall`.

Codespaces public ports are reachable from anywhere with the URL,
so do not skip step 3 — without `OPENMEMORY_HTTP_TOKEN` the server
logs a warning and serves the world. Codespaces also auto-suspend
after 30 minutes idle; the URL stays the same on resume but you'll
need to relaunch the server.

## Reporting issues

Open a GitHub issue with:

- The exact command + feature flags you ran.
- What you expected vs. what happened.
- Output of `openmemory status --json` (memory state) and the
  relevant subset of `~/server.log` if the HTTP transport is
  involved (mask any bearer tokens before pasting).

For security-sensitive reports (auth bypass, integrity-check
escape, anything that smells like a vulnerability), please email
the maintainer directly rather than filing a public issue.
