#!/usr/bin/env bash
# Instruction-count benchmarks with a 3% regression check (decision D-015).
#
#   tools/bench.sh check            run the benchmarks, compare with the baseline
#   tools/bench.sh check --write    same, then store the new counts (the gate)
#
# Counts come from Valgrind (Callgrind) through gungraun, so they are
# deterministic for a given binary. Different rustc or Valgrind versions give
# different counts, so the baseline records the toolchain. When it doesn't
# match this machine, the script measures origin/main here with this
# toolchain and compares against that instead.
#
# Needs valgrind and gungraun-runner (the version in Cargo.lock):
#   cargo install gungraun-runner --version <version> --locked
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
baseline="$root/conformance/bench-baseline.txt"
cmd="${1:-check}"
write="${2:-}"
[ "$cmd" = check ] || { echo "usage: tools/bench.sh check [--write]"; exit 2; }
[ -z "$write" ] || [ "$write" = --write ] || { echo "usage: tools/bench.sh check [--write]"; exit 2; }

want="$(sed -n '/^name = "gungraun"$/{n;s/^version = "\(.*\)"$/\1/p}' "$root/Cargo.lock")"
have="$(gungraun-runner --version 2>/dev/null | awk '{print $2}' || true)"
if [ "$want" != "$have" ]; then
  echo "need gungraun-runner $want (found: ${have:-none}):"
  echo "  cargo install gungraun-runner --version $want --locked"
  exit 2
fi
command -v valgrind >/dev/null || { echo "need valgrind"; exit 2; }

toolchain="$(rustc -V) / $(valgrind --version)"

# Runs the benchmarks of the checkout at $1 with target dir $2; prints the
# directory holding the summaries.
measure() {
  local src="$1" target="$2"
  rm -rf "$target/gungraun/ferrojs-bench"
  (cd "$src" && CARGO_TARGET_DIR="$target" cargo bench --locked -q -p ferrojs-bench \
    -- --save-summary=json >/dev/null)
  echo "$target/gungraun/ferrojs-bench"
}

cargo build --release --locked -q -p ferrojs-bench --bin benchcheck
check="$root/target/release/benchcheck"

echo "Benchmarks (instruction counts, $toolchain)"
results="$(measure "$root" "$root/target")"
status=0
"$check" --summaries "$results" --baseline "$baseline" --toolchain "$toolchain" $write || status=$?

if [ "$status" = 3 ]; then
  echo "measuring origin/main with this toolchain for a comparable baseline..."
  tmp="$(mktemp -d)"
  trap 'git -C "$root" worktree remove --force "$tmp/main" >/dev/null 2>&1 || true; rm -rf "$tmp"' EXIT
  git -C "$root" worktree add -q --detach "$tmp/main" origin/main
  if [ -d "$tmp/main/crates/ferrojs-bench" ]; then
    main_results="$(measure "$tmp/main" "$tmp/target")"
    "$check" --summaries "$main_results" --baseline "$tmp/baseline.txt" \
      --toolchain "$toolchain" --write >/dev/null
    status=0
    "$check" --summaries "$results" --baseline "$tmp/baseline.txt" --toolchain "$toolchain" || status=$?
  else
    echo "origin/main has no benchmarks yet; nothing to compare"
    status=0
  fi
  if [ "$status" = 0 ] && [ "$write" = --write ]; then
    rm -f "$baseline"
    "$check" --summaries "$results" --baseline "$baseline" --toolchain "$toolchain" --write
  fi
fi
exit "$status"
