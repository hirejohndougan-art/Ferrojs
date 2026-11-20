#!/usr/bin/env bash
# Dependency checks (decision D-026): licenses, known vulnerabilities,
# yanked or unmaintained crates, and where crates come from.
# Uses deny.toml at the repository root.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if ! command -v cargo-deny >/dev/null 2>&1; then
  echo "cargo-deny is not installed. Install it with:" >&2
  echo "  cargo install cargo-deny --locked" >&2
  exit 1
fi

cargo deny --all-features check advisories bans licenses sources
