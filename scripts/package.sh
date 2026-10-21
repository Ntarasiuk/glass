#!/bin/sh
# Builds release tarballs into dist/: macOS universal (Apple Silicon + Intel) and, with
# Docker running, Linux x86_64 (built in an Arch container).
# Layout: glass/bin/glass, glass/share/glass/web/*
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release -p glass-host --target "$target"
done

# tarball <platform> <binary>
tarball() {
  stage=$(mktemp -d)
  mkdir -p "$stage/glass/bin" "$stage/glass/share/glass"
  cp "$2" "$stage/glass/bin/glass"
  cp -R web "$stage/glass/share/glass/web"
  mkdir -p dist
  name="glass-$version-$1.tar.gz"
  tar -C "$stage" -czf "dist/$name" glass
  rm -rf "$stage"
  (cd dist && shasum -a 256 "$name" > "$name.sha256" && cp "$name" "glass-latest-$1.tar.gz")
  echo "dist/$name ($(du -h "dist/$name" | cut -f1))"
}

universal=$(mktemp)
lipo -create -output "$universal" \
  target/aarch64-apple-darwin/release/glass-host \
  target/x86_64-apple-darwin/release/glass-host
tarball darwin-universal "$universal"
rm -f "$universal"

if docker info >/dev/null 2>&1; then
  scripts/linux/build.sh
  tarball linux-x86_64 target/linux-x86_64/release/glass-host
else
  echo "skipping Linux build (Docker isn't running)"
fi
