#!/usr/bin/env bash
# Build the browser workbench and stage it for `wrangler deploy`.
# Usage: deploy/web/sync.sh   (from the repository root; needs trunk)
set -euo pipefail
cd "$(dirname "$0")/../.."
(cd crates/optcoil-app && trunk build --release)
rm -rf deploy/web/public
mkdir -p deploy/web/public
cp -R crates/optcoil-app/dist/. deploy/web/public/
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
