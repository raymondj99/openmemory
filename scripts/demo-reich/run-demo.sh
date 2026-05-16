#!/usr/bin/env bash
# End-to-end harness for IMPLEMENTATION.md's Reich molecular-sex demo.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEMO_ROOT="${DEMO_ROOT:-/tmp/openmemory-reich-demo}"
OPENMEMORY_HOME="${OPENMEMORY_HOME:-$DEMO_ROOT/om-home}"
OM="${OM:-$REPO_ROOT/target/release/openmemory}"
PYBIN="$DEMO_ROOT/.venv/bin/python"
TRANSCRIPT=""
PRESERVED_TRANSCRIPT=""
USE_REFERENCE=0
SKIP_BUILD=0

usage() {
    cat <<'EOF'
Usage: scripts/demo-reich/run-demo.sh [--transcript PATH | --reference] [--skip-build]

Builds the openmemory binary, recreates /tmp/openmemory-reich-demo,
generates the labshare tree and synthetic BAMs, seeds openmemory, and
starts the MCP HTTP server plus filesystem watcher.

--transcript PATH  Extract the first fenced bash block to artifacts/molecular_sex.sh.
--reference        Grade the committed reference script instead of an agent transcript.
--skip-build       Reuse target/release/openmemory.
EOF
}

while (($#)); do
    case "$1" in
        --transcript)
            TRANSCRIPT="${2:-}"
            if [[ -z "$TRANSCRIPT" ]]; then
                printf 'error: --transcript requires a path\n' >&2
                exit 1
            fi
            shift 2
            ;;
        --reference)
            USE_REFERENCE=1
            shift
            ;;
        --skip-build)
            SKIP_BUILD=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'error: unknown argument: %s\n' "$1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

for cmd in cargo jq curl python3 samtools; do
    if ! command -v "$cmd" > /dev/null; then
        printf 'error: required command not found: %s\n' "$cmd" >&2
        exit 1
    fi
done

if ((SKIP_BUILD == 0)); then
    cargo build --release --bin openmemory
fi

if ! "$OM" watch --help > "$DEMO_ROOT.watch-help.tmp" 2>&1; then
    if ! grep -q 'unrecognized subcommand' "$DEMO_ROOT.watch-help.tmp"; then
        cat "$DEMO_ROOT.watch-help.tmp" >&2
        rm -f "$DEMO_ROOT.watch-help.tmp"
        exit 1
    fi
fi
if grep -q 'unrecognized subcommand' "$DEMO_ROOT.watch-help.tmp"; then
    rm -f "$DEMO_ROOT.watch-help.tmp"
    cargo build --release --bin openmemory --features watch
fi
rm -f "$DEMO_ROOT.watch-help.tmp"

if [[ -n "$TRANSCRIPT" ]]; then
    PRESERVED_TRANSCRIPT="$(mktemp "${TMPDIR:-/tmp}/reich-transcript.XXXXXX")"
    cp "$TRANSCRIPT" "$PRESERVED_TRANSCRIPT"
    TRANSCRIPT="$PRESERVED_TRANSCRIPT"
fi

for pid_file in "$DEMO_ROOT/artifacts/watch.pid" "$DEMO_ROOT/artifacts/mcp.pid"; do
    if [[ -f "$pid_file" ]]; then
        pid="$(cat "$pid_file")"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
    fi
done

rm -rf "$DEMO_ROOT"
mkdir -p "$DEMO_ROOT"
export DEMO_ROOT OPENMEMORY_HOME OM

python3 -m venv "$DEMO_ROOT/.venv"
"$DEMO_ROOT/.venv/bin/pip" install --quiet pysam numpy

"$SCRIPT_DIR/build-labshare.sh"
DEMO_ROOT="$DEMO_ROOT" "$PYBIN" "$SCRIPT_DIR/make_synthetic_bams.py"
"$SCRIPT_DIR/seed.sh"

if [[ -n "$TRANSCRIPT" ]]; then
    cp "$TRANSCRIPT" "$DEMO_ROOT/artifacts/transcript.md"
    awk '/^```bash$/{flag=1;next}/^```$/{if(flag){flag=0; exit}}flag' \
        "$DEMO_ROOT/artifacts/transcript.md" \
        > "$DEMO_ROOT/artifacts/molecular_sex.sh"
    chmod +x "$DEMO_ROOT/artifacts/molecular_sex.sh"
elif ((USE_REFERENCE == 1)); then
    cp "$SCRIPT_DIR/reference_molecular_sex.sh" "$DEMO_ROOT/artifacts/molecular_sex.sh"
    chmod +x "$DEMO_ROOT/artifacts/molecular_sex.sh"
else
    cat <<EOF
Demo memory is seeded and ready.

MCP HTTP endpoint: http://127.0.0.1:7801/mcp
Prompt: $REPO_ROOT/PROMPT.md

After capturing an agent transcript, run:
  $0 --skip-build --transcript "$DEMO_ROOT/artifacts/transcript.md"

For a local smoke test without an agent, run:
  $0 --skip-build --reference
EOF
    exit 0
fi

"$SCRIPT_DIR/grade.sh" "$DEMO_ROOT/artifacts/molecular_sex.sh"

if [[ -n "$PRESERVED_TRANSCRIPT" ]]; then
    rm -f "$PRESERVED_TRANSCRIPT"
fi
