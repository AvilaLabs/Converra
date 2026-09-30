#!/usr/bin/env bash
# Build Converra.app from a compiled optcoil-app binary. macOS only —
# uses sips and iconutil, both shipped with the OS.
#
#   packaging/make-app.sh <optcoil-app binary> <version>
#
# Produces ./Converra.app ready for codesign/notarytool.
set -euo pipefail

BINARY="${1:?path to the compiled optcoil-app binary}"
VERSION="${2:?version string, e.g. 0.2.0}"
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"

APP="Converra.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BINARY" "$APP/Contents/MacOS/Converra"
chmod +x "$APP/Contents/MacOS/Converra"
sed "s/{{VERSION}}/$VERSION/g" "$HERE/Info.plist" > "$APP/Contents/Info.plist"

# .icns from the bundled Avila Labs logo via a .iconset.
ICONSET="$(mktemp -d)/icon.iconset"
mkdir -p "$ICONSET"
SRC="$ROOT/crates/optcoil-app/assets/avila-labs-logo.png"
for px in 16 32 128 256 512; do
    sips -z "$px" "$px" "$SRC" --out "$ICONSET/icon_${px}x${px}.png" >/dev/null
    dbl=$((px * 2))
    sips -z "$dbl" "$dbl" "$SRC" --out "$ICONSET/icon_${px}x${px}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"

echo "Built $APP (version $VERSION)"
