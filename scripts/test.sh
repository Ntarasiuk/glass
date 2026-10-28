#!/bin/sh
# Runs every test suite. Skips the Linux build when Docker isn't running.
#   scripts/test.sh            all suites
#   scripts/test.sh e2e        just the browser end-to-end tests
set -eu
cd "$(dirname "$0")/.."
only=${1:-all}
step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

if [ "$only" = all ] || [ "$only" = rust ]; then
  step "Rust unit tests"
  cargo test --workspace
fi
if [ "$only" = all ] || [ "$only" = js ]; then
  step "JS unit tests"
  node --test tests/unit/*.test.mjs
fi
if [ "$only" = all ] || [ "$only" = e2e ]; then
  step "End-to-end (Chrome against glass --test)"
  # The `ci` profile (Cargo.toml) is optimized like release but builds in seconds; with
  # incremental compilation on, an edit rebuilds in about 3 s (release: about 2 min).
  CARGO_INCREMENTAL=1 cargo build --profile ci -p glass-host
  (cd tests/e2e && npm ci --silent && GLASS_BIN="$PWD/../../target/ci/glass-host" npx playwright test)
fi
if [ "$only" = all ] || [ "$only" = site ]; then
  step "Site and API"
  (cd site && npm ci --silent)
  node --test tests/site/site.test.mjs
fi
if [ "$only" = all ] || [ "$only" = linux ]; then
  if docker info >/dev/null 2>&1; then
    step "Linux build and tests (Arch container)"
    scripts/linux/build.sh test
    step "Linux installer"
    tests/installer/linux.sh
  else
    step "Linux: skipped (Docker isn't running)"
  fi
fi
printf '\n\033[32mAll tests passed.\033[0m\n'
