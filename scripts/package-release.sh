#!/bin/sh
set -eu
platform=${1:?Usage: package-release.sh PLATFORM}
case "$platform" in darwin-arm64|darwin-x64|linux-arm64|linux-x64) ;; *) echo 'Unsupported platform' >&2; exit 1;; esac
cargo build --release --locked
# Recreate only this generated staging directory to avoid stale release files.
rm -rf "release/editor-bridge-$platform"
mkdir -p "release/editor-bridge-$platform"
cp target/release/bridge target/release/mcp-editor-lsp-bridge "release/editor-bridge-$platform/"
cp -R skills docs "release/editor-bridge-$platform/"
mkdir -p "release/editor-bridge-$platform/zed-extension"
cp -R zed-extension/src zed-extension/Cargo.toml zed-extension/Cargo.lock zed-extension/extension.toml "release/editor-bridge-$platform/zed-extension/"
cp web/ATTRIBUTION.md "release/editor-bridge-$platform/ATTRIBUTION.md"
cp README.md "release/editor-bridge-$platform/"
tar -czf "release/editor-bridge-$platform.tar.gz" -C release "editor-bridge-$platform"
