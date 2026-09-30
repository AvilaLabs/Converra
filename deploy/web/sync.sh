#!/usr/bin/env bash
# Build the browser workbench and stage it for `wrangler deploy`.
# Usage: deploy/web/sync.sh [verified-build-directory]
# Without an argument, build into a unique directory so concurrent dev builds
# cannot replace Trunk's staging files. An argument stages an already checked
# bundle without changing it.
set -euo pipefail
cd "$(dirname "$0")/../.."
if [[ $# -gt 1 ]]; then
  echo "Usage: deploy/web/sync.sh [verified-build-directory]" >&2
  exit 2
fi
if [[ $# -eq 1 ]]; then
  build_dist="$(cd "$1" && pwd)"
else
  mkdir -p runs
  build_directory="$(mktemp -d "$(pwd)/runs/web-build.XXXXXX")"
  trap 'rm -rf "$build_directory"' EXIT
  build_dist="$build_directory/dist"
  (cd crates/optcoil-app && env -u NO_COLOR trunk build --release --dist "$build_dist")
fi
for asset in index.html optcoil-app.js optcoil-app_bg.wasm worker.js; do
  [[ -f "$build_dist/$asset" ]] || { echo "Missing browser asset: $asset" >&2; exit 1; }
done
rm -rf deploy/web/public
mkdir -p deploy/web/public
cp -R "$build_dist/." deploy/web/public/
python3 - <<'PY'
import hashlib, json, pathlib, subprocess, tomllib

root = pathlib.Path.cwd()
public = root / "deploy/web/public"
manifest = {
    "schema": "converra-browser-release/v1",
    "version": tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"],
    "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "source_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], text=True).strip()),
    "assets": {
        str(path.relative_to(public)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(public.rglob("*")) if path.is_file() and path.name != "release.json"
    },
}
(public / "release.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(f"Staged {len(manifest['assets'])} assets for Converra {manifest['version']} in {public}")
PY
