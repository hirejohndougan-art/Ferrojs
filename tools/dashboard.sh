#!/usr/bin/env bash
# Writes docs/reports/dashboard.md: Test262 pass counts per directory and the
# benchmark instruction counts (roadmap task 0.5).
#
#   tools/dashboard.sh
#
# Runs the whole Test262 suite on FerroJS (fetching the pinned checkout if
# needed) and reads the benchmark numbers from conformance/bench-baseline.txt,
# which the gate keeps current. Run it after the gate passes, so the page
# matches what was merged.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

tools/fetch-test262.sh >/dev/null
cargo build --release --locked -q -p ferrojs-test262 --bins
report="$root/target/test262-report.json"
target/release/test262 --suite test262 --out "$report" >/dev/null
target/release/dashboard \
  --report "$report" \
  --bench conformance/bench-baseline.txt \
  --source "$(git rev-parse --short HEAD) ($(git log -1 --format=%cs))" \
  --out docs/reports/dashboard.md
