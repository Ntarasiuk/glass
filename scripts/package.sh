#!/bin/sh
# Builds a universal (Apple Silicon + Intel) release tarball into dist/.
# Layout: glass/bin/glass, glass/share/glass/web/*
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release -p glass-host --target "$target"
done

stage=$(mktemp -d)
mkdir -p "$stage/glass/bin" "$stage/glass/share/glass"
lipo -create -output "$stage/glass/bin/glass" \
  target/aarch64-apple-darwin/release/glass-host \
  target/x86_64-apple-darwin/release/glass-host
cp -R web "$stage/glass/share/glass/web"

mkdir -p dist
name="glass-$version-darwin-universal.tar.gz"
tar -C "$stage" -czf "dist/$name" glass
rm -rf "$stage"
(cd dist && shasum -a 256 "$name" > "$name.sha256" && cp "$name" glass-latest-darwin-universal.tar.gz)
echo "dist/$name ($(du -h "dist/$name" | cut -f1))"
