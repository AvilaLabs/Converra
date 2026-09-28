#!/usr/bin/env bash
# Build the browser workbench and stage it for `wrangler deploy`.
# Usage: deploy/web/sync.sh   (from the repository root; needs trunk)
set -euo pipefail
cd "$(dirname "$0")/../.."
cargo build --release --target wasm32-unknown-unknown -p optcoil-app 2>/dev/null || true
(cd crates/optcoil-app && trunk build --release)
rm -rf deploy/web/public
mkdir -p deploy/web/public
cp crates/optcoil-app/dist/* deploy/web/public/
echo "Staged $(ls deploy/web/public | wc -l) files in deploy/web/public/"
