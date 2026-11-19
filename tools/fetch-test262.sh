#!/usr/bin/env bash
# Downloads Test262 at the commit pinned in tools/test262-commit.txt into
# ./test262 (or the directory given as the first argument).
#
# Pinning keeps results reproducible: a new upstream test never changes our
# numbers by surprise. Update the pin deliberately, in its own commit, and
# refresh the baseline in the same commit.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="${1:-$root/test262}"
commit="$(tr -d '[:space:]' < "$root/tools/test262-commit.txt")"

if [ -d "$dest/.git" ] && [ "$(git -C "$dest" rev-parse HEAD 2>/dev/null)" = "$commit" ]; then
  echo "test262 already at $commit"
  exit 0
fi

rm -rf "$dest"
mkdir -p "$dest"
git -C "$dest" init -q
git -C "$dest" config core.autocrlf false  # some tests depend on exact line endings
git -C "$dest" remote add origin https://github.com/tc39/test262.git
git -C "$dest" fetch -q --depth 1 origin "$commit"
git -C "$dest" checkout -q FETCH_HEAD
echo "test262 checked out at $commit"
