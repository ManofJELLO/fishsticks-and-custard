#!/bin/bash
# Build FishsticksAndCustard.app from a release build, and zip it.
#
#   packaging/macos/build-app.sh <version> <out dir>
#
# Needs (as the CI macOS job has them): `cargo build --release` done,
# Qt's bin on PATH (macdeployqt), and Homebrew's espeak-ng and librsvg.
set -euo pipefail

version="$1"
out="$2"
root="$(cd "$(dirname "$0")/../.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}/release"
app="$out/FishsticksAndCustard.app"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

# The programs: fscd (the app) and fsc (the command line; the cask links it).
cp "$target/fscd" "$target/fsc" "$app/Contents/MacOS/"
sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"

# The icon, from the same SVG as the Linux one.
iconset="$out/AppIcon.iconset"
rm -rf "$iconset" && mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$root/packaging/desktop/dev.fsc.FishsticksAndCustard.svg" -o "$iconset/icon_${size}x${size}.png"
    rsvg-convert -w "$((size * 2))" -h "$((size * 2))" "$root/packaging/desktop/dev.fsc.FishsticksAndCustard.svg" -o "$iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

# espeak-ng's phoneme data, which the voices need (fsc-audio looks in
# Resources first).
cp -R "$(brew --prefix espeak-ng)/share/espeak-ng-data" "$app/Contents/Resources/"

# Qt's frameworks and the QML modules the popup and menu bar import.
macdeployqt "$app" -qmldir="$root/crates/fsc-app/qml" -always-overwrite -verbose=1
# Database drivers come along with Qt's plugins; fscd uses no database.
rm -rf "$app/Contents/PlugIns/sqldrivers"

# Nothing may point at this machine's build or Homebrew paths. (`otool -L`
# names the file first; its libraries follow, indented.)
for bin in "$app/Contents/MacOS/fscd" "$app/Contents/MacOS/fsc"; do
    if otool -L "$bin" | tail -n +2 | grep -E "/Users/|/opt/homebrew/|/usr/local/"; then
        echo "error: $(basename "$bin") links to libraries outside the app (above)" >&2
        exit 1
    fi
done

# Apple Silicon runs only signed code: sign it ad hoc (no developer ID).
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

# It starts, with the bundled Qt: draw the tray icons and quit.
"$app/Contents/MacOS/fscd" --dump-icons "$out/icons-check" >/dev/null
rm -rf "$out/icons-check"

zip="$out/FishsticksAndCustard-$version.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
shasum -a 256 "$zip" | tee "$zip.sha256"
