#!/usr/bin/env bash
# Builds and runs the libFuzzer targets in fuzz/ on stable Rust (D-014).
#
#   tools/fuzz.sh smoke [seconds]        every target for a few seconds (the gate)
#   tools/fuzz.sh run <target> [seconds] one target; 0 seconds = until stopped
#   tools/fuzz.sh list                   target names
#
# Coverage instrumentation comes from stable rustc flags, so no nightly or
# cargo-fuzz is needed. For AddressSanitizer runs, use nightly cargo-fuzz:
#   cd fuzz && cargo +nightly fuzz run <target>
#
# Long runs keep their corpus in fuzz/corpus/<target>/ (git-ignored) and
# write crashing inputs to fuzz/artifacts/<target>/. A crash found by a long
# run gets fixed with a regression test, like any other bug.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
fuzz="$root/fuzz"
cmd="${1:-smoke}"
host="$(rustc -vV | sed -n 's/^host: //p')"

targets() {
  sed -n 's/^name = "\(.*\)"$/\1/p' "$fuzz/Cargo.toml" | grep -v '^ferrojs-fuzz$'
}

build() {
  # Same instrumentation cargo-fuzz uses, minus the sanitizer.
  local flags="--cfg fuzzing -Cpasses=sancov-module"
  flags+=" -Cllvm-args=-sanitizer-coverage-level=4"
  flags+=" -Cllvm-args=-sanitizer-coverage-inline-8bit-counters"
  flags+=" -Cllvm-args=-sanitizer-coverage-pc-table"
  flags+=" -Cllvm-args=-sanitizer-coverage-trace-compares"
  # An explicit --target keeps the flags off build scripts and proc macros.
  (cd "$fuzz" && RUSTFLAGS="$flags" cargo build --release --locked --bins \
    --target "$host" --target-dir "$fuzz/target" --quiet)
}

run_one() {
  local target="$1" secs="$2" corpus="$3"
  local bin="$fuzz/target/$host/release/$target"
  mkdir -p "$corpus" "$fuzz/artifacts/$target"
  local args=(-artifact_prefix="$fuzz/artifacts/$target/" -print_final_stats=1)
  if [ "$secs" != 0 ]; then args+=(-max_total_time="$secs"); fi
  echo "== $target (${secs}s)"
  local log status=0
  log="$(mktemp)"
  "$bin" "${args[@]}" "$corpus" >"$log" 2>&1 || status=$?
  if [ "$status" = 0 ]; then
    grep -E '^stat::(number_of_executed_units|new_units_added)' "$log" || true
  else
    tail -n 40 "$log"
  fi
  rm -f "$log"
  return "$status"
}

case "$cmd" in
  list) targets ;;
  smoke)
    secs="${2:-10}"
    # The fuzz crate is outside the workspace, so lint it here.
    (cd "$fuzz" && cargo fmt --check && cargo clippy --locked --bins --quiet -- -D warnings)
    build
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    status=0
    for t in $(targets); do
      # Fresh corpus each time, so the gate never edits the repo.
      run_one "$t" "$secs" "$tmp/$t" || status=1
    done
    if [ "$status" != 0 ]; then echo "fuzz smoke: FAILED (see fuzz/artifacts/)"; fi
    exit "$status"
    ;;
  run)
    target="${2:?usage: tools/fuzz.sh run <target> [seconds]}"
    targets | grep -qx "$target" || { echo "unknown target: $target"; exit 2; }
    build
    run_one "$target" "${3:-0}" "$fuzz/corpus/$target"
    ;;
  *) echo "usage: tools/fuzz.sh smoke [seconds] | run <target> [seconds] | list"; exit 2 ;;
esac
