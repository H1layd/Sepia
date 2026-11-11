#!/usr/bin/env bash
# Build the native pieces and run every test suite we have.
#
#   scripts/build.sh            # build rust core + c++ probe, run unit tests
#   scripts/build.sh --install  # also drop the binaries into ~/.sepia/bin
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

say() { printf '\n== %s ==\n' "$*"; }

say "python syntax"
python3 -m compileall -q sepia tests

say "python unit tests"
python3 -m unittest discover -s tests -q

say "rust core"
if command -v cargo >/dev/null 2>&1; then
  cargo build --release --manifest-path core/Cargo.toml
  cargo test --release --manifest-path core/Cargo.toml --quiet
else
  echo "cargo not found: skipping the rust engine"
fi

say "from-scratch browser"
if command -v cargo >/dev/null 2>&1; then
  cargo build --release --manifest-path browser/Cargo.toml
  cargo test --release --manifest-path browser/Cargo.toml --quiet
else
  echo "cargo not found: skipping sepia-browser"
fi

say "c++ probe"
CXX="${CXX:-c++}"
if command -v "$CXX" >/dev/null 2>&1; then
  "$CXX" -std=c++17 -O2 -Wall -Wextra -o native/sepiaprobe native/sepiaprobe.cpp
  ./native/sepiaprobe --sni example.com >/dev/null
else
  echo "$CXX not found: skipping sepiaprobe"
fi

if [ "${1:-}" = "--install" ]; then
  say "install binaries"
  install -d "$HOME/.sepia/bin"
  [ -x core/target/release/sepia-core ] && install -m 0755 core/target/release/sepia-core "$HOME/.sepia/bin/"
  [ -x native/sepiaprobe ] && install -m 0755 native/sepiaprobe "$HOME/.sepia/bin/"
  [ -x browser/target/release/sepia-browser ] && install -m 0755 browser/target/release/sepia-browser "$HOME/.sepia/bin/"
  echo "installed into $HOME/.sepia/bin (add it to PATH if you want sepia-core there)"
fi

say "integration (needs network)"
if [ -x core/target/release/sepia-core ]; then
  bash tests/core_e2e.sh || echo "core integration test reported failures"
fi

say "done"
