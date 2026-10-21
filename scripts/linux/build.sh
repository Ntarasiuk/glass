#!/bin/sh
# Builds (and tests) the Linux x86_64 host in an Arch container.
#   scripts/linux/build.sh          build release
#   scripts/linux/build.sh test     run tests
set -eu
cd "$(dirname "$0")/../.."
docker build --platform linux/amd64 -t glass-linux-build scripts/linux >/dev/null
mode=${1:-build}
cmd="cargo build --release -p glass-host"
[ "$mode" = test ] && cmd="cargo test -p glass-host -p glass-proto"
docker run --rm --platform linux/amd64 \
  -v "$PWD":/src -v glass-cargo-registry:/root/.cargo/registry \
  -e CARGO_TARGET_DIR=/src/target/linux-x86_64 \
  glass-linux-build sh -c "$cmd"
