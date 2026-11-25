#!/usr/bin/env bash
# Checks that interpreter dispatch doesn't get slower as ops are added
# (decision D-052): runs the `interp` benchmarks as built normally and with
# the `padding-ops` feature (150 extra ops that are never executed), and
# fails if any benchmark's instruction count differs by more than 1%.
#
# Needs valgrind and gungraun-runner, like tools/bench.sh.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
limit="${FERROJS_DISPATCH_LIMIT:-1.0}"

command -v valgrind >/dev/null || { echo "dispatch-flat: need valgrind"; exit 2; }

# The padded build really has 150 more ops (else both runs would measure
# the same thing and the check would pass vacuously).
cargo test --locked -q -p ferrojs-bytecode --features padding-ops --test padding

# Prints "<benchmark id> <instructions>" per benchmark; on a failed run,
# shows cargo's output and stops.
measure() {
  local out
  # Plain text: CI sets CARGO_TERM_COLOR=always (dtolnay/rust-toolchain),
  # and gungraun's colour codes would hide the lines awk looks for.
  if ! out="$(CARGO_TERM_COLOR=never cargo bench --locked -q -p ferrojs-bench --bench interp "$@" 2>&1)"; then
    echo "$out"
    echo "dispatch-flat: the benchmark run failed ($*)"
    exit 1
  fi
  awk '/^interp::/{name=$1" "$2} /^ *Instructions:/{split($2,a,"|"); print name, a[1]}' <<<"$out"
}

plain="$(measure)"
padded="$(measure --features padding-ops)"
[ -n "$plain" ] || { echo "dispatch-flat: no benchmark results"; exit 1; }

echo "Dispatch against the op count (instructions; +150 unused ops):"
status=0
while read -r id case count; do
  other="$(awk -v id="$id" -v c="$case" '$1==id && $2==c {print $3}' <<<"$padded")"
  if [ -z "$other" ]; then
    echo "  missing $id $case in the padded run"
    status=1
    continue
  fi
  verdict="$(awk -v a="$count" -v b="$other" -v l="$limit" \
    'BEGIN{d=(b-a)*100/a; printf "%s %+.2f%%", (d>l||d< -l)?"FAIL":"ok", d}')"
  echo "  ${verdict%% *}  $id $case: $count -> $other (${verdict#* })"
  [ "${verdict%% *}" = ok ] || status=1
done <<<"$plain"
[ "$status" = 0 ] || echo "dispatch cost depends on the number of ops (limit ${limit}%, D-052)"
exit "$status"
