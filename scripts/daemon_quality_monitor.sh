#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run() {
  printf '\n[daemon-monitor] %s\n' "$*"
  "$@"
}

check_no_matches() {
  local description="$1"
  local pattern="$2"
  local matches
  local status
  local -a paths=()
  local exclude=""
  shift 2
  while [[ "$#" -gt 0 ]]; do
    case "$1" in
      --exclude)
        exclude="$2"
        shift 2
        ;;
      *)
        paths+=("$1")
        shift
        ;;
    esac
  done
  matches="$(mktemp)"
  printf '\n[daemon-monitor] checking %s\n' "$description"
  set +e
  if command -v rg >/dev/null 2>&1; then
    local -a rg_args=(-n "$pattern")
    if [[ -n "$exclude" ]]; then
      rg_args+=(--glob "!$exclude")
    fi
    rg "${rg_args[@]}" "${paths[@]}" >"$matches"
  else
    local -a grep_args=(-R -n -E "$pattern")
    if [[ -n "$exclude" ]]; then
      grep_args+=(--exclude="$exclude")
    fi
    grep "${grep_args[@]}" "${paths[@]}" >"$matches"
  fi
  status=$?
  set -e
  if [[ "$status" -eq 0 ]]; then
    cat "$matches"
    rm -f "$matches"
    printf '[daemon-monitor] failed: %s\n' "$description" >&2
    exit 1
  elif [[ "$status" -ne 1 ]]; then
    rm -f "$matches"
    printf '[daemon-monitor] search failed while checking %s\n' "$description" >&2
    exit "$status"
  fi
  rm -f "$matches"
}

run git diff --check

check_no_matches \
  "no unfinished daemon wiring markers" \
  "not wired yet|TODO|FIXME" crates/openmemory-admin crates/openmemory-daemon crates/openmemory-cli/src/commands/daemon.rs

check_no_matches \
  "no panic/unwrap/expect on daemon non-test request paths" \
  "(\\.unwrap\\(|\\.expect\\(|panic!\\()" crates/openmemory-daemon/src \
  --exclude tests.rs

run cargo fmt --all -- --check
run cargo test -p openmemory-admin -p openmemory-daemon --all-features
run cargo test -p openmemory-daemon --no-default-features
run cargo test -p openmemory-cli --test cli_output --all-features
run cargo test -p openmemory-cli --test cli_output --no-default-features
run cargo test --workspace --all-features
run cargo test --workspace --no-default-features
run cargo clippy --workspace --all-features --all-targets -- -D warnings
run cargo clippy --workspace --no-default-features --all-targets -- -D warnings
run env "RUSTDOCFLAGS=-D warnings" cargo doc --workspace --no-deps --all-features
run cargo build --workspace --locked
run cargo deny check

if [[ "${OPENMEMORY_DAEMON_MONITOR_BENCH:-0}" == "1" ]]; then
  bench_baseline="daemon-monitor-$(date +%Y%m%d%H%M%S)-$$"
  run cargo bench -p openmemory-bench --bench openmemory -- daemon_admin_api --save-baseline "$bench_baseline"
else
  printf '\n[daemon-monitor] benchmark run skipped; set OPENMEMORY_DAEMON_MONITOR_BENCH=1 to run daemon_admin_api locally with a per-run daemon-monitor baseline\n'
fi

printf '\n[daemon-monitor] daemon production gate passed\n'
