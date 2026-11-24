#!/usr/bin/env bash
# Builds the Rust release and packs it into the NSIS installer with electron-builder (prepackaged: no Electron).
# Needs `npm install` in rust/installer once. Output: rust/dist/Spark Setup <version>.exe.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"
VERSION="${VERSION:-2.0.0}"
(cd rust/app && cargo build --release)
STAGE=rust/installer/stage; rm -rf "$STAGE"; mkdir -p "$STAGE"
cp rust/app/target/release/Spark.exe "$STAGE/Spark.exe"
# electron-builder reads rust/installer/package.json; paths below are relative to rust/installer
(cd rust/installer && npx electron-builder --win nsis --prepackaged "../../$STAGE" \
  -c.extraMetadata.version="$VERSION" -c.directories.output=../dist \
  -c.nsis.packElevateHelper=false -c.nsis.include=installer.nsh)
rm -rf "$STAGE" rust/dist/builder-debug.yml rust/dist/latest.yml rust/dist/*.blockmap
ls -la rust/dist/*.exe
