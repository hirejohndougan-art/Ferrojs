#!/usr/bin/env bash
# Runs ferrojs-core's tests under Miri (decision D-016).
#
# Miri needs nightly Rust with the `miri` component:
#   rustup toolchain install nightly --component miri
#
# Some run environments can't install nightly. There, the script only
# passes if ferrojs-core is unchanged against origin/main (Miri can't have
# anything new to say). If ferrojs-core changed, it fails and says how to
# get the result from CI instead: push the branch and wait for the `miri`
# job to pass before merging. Set FERROJS_MIRI_REQUIRED=1 (CI does) to
# fail whenever Miri can't run.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# Strict provenance: the heap cage hands out 32-bit offsets (D-002); every
# pointer must be derived from the cage base, never from an integer.
export MIRIFLAGS="${MIRIFLAGS:--Zmiri-strict-provenance -Zmiri-symbolic-alignment-check}"

if cargo +nightly miri --version >/dev/null 2>&1; then
  cargo +nightly miri setup >/dev/null 2>&1 || cargo +nightly miri setup
  log="$(mktemp)"
  trap 'rm -f "$log"' EXIT
  status=0
  cargo +nightly miri test -p ferrojs-core 2>&1 | tee "$log" || status=$?
  [ "$status" = 0 ] || { echo "miri: FAILED"; exit "$status"; }
  if ! grep -q 'running_under_miri ... ok' "$log"; then
    echo "miri: the canary test didn't run; Miri isn't really in use"
    exit 1
  fi
  echo "miri: ok ($MIRIFLAGS)"
  exit 0
fi

if [ "${FERROJS_MIRI_REQUIRED:-}" = 1 ]; then
  echo "miri: nightly with the miri component is not installed"
  exit 1
fi

if git rev-parse -q --verify origin/main >/dev/null &&
  git diff --quiet origin/main -- crates/ferrojs-core &&
  [ -z "$(git status --porcelain --untracked-files=all -- crates/ferrojs-core)" ]; then
  echo "miri: skipped (no nightly here; ferrojs-core unchanged against origin/main)"
  exit 0
fi

cat <<'MSG'
miri: ferrojs-core changed, but nightly Miri isn't available here.
  Push this branch (git push origin HEAD) and wait for the CI `miri` job on
  it to pass before merging. Record "Miri: passed in CI run <id>" in the
  progress entry.
MSG
exit 1
