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
  shift
  printf '\n[daemon-monitor] checking %s\n' "$description"
  if rg "$@" >/tmp/openmemory-daemon-monitor.matches; then
    cat /tmp/openmemory-daemon-monitor.matches
    rm -f /tmp/openmemory-daemon-monitor.matches
    printf '[daemon-monitor] failed: %s\n' "$description" >&2
    exit 1
  fi
  rm -f /tmp/openmemory-daemon-monitor.matches
}

run git diff --check

check_no_matches \
  "no unfinished daemon wiring markers" \
  -n "not wired yet|TODO|FIXME" crates/openmemory-admin crates/openmemory-daemon crates/openmemory-cli/src/commands/daemon.rs

check_no_matches \
  "no panic/unwrap/expect on daemon non-test request paths" \
  -n "(\\.unwrap\\(|\\.expect\\(|panic!\\()" crates/openmemory-daemon/src \
  --glob '!tests.rs'

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
