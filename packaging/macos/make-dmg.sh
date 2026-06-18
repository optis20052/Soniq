#!/usr/bin/env bash
#
# Build a fully self-contained Soniq.app and package it into a drag-to-install
# .dmg. "Self-contained" = libmpv and every non-system dylib it pulls in are
# copied INTO the bundle and their load paths rewritten to @executable_path,
# so the end user needs no Homebrew / mpv / anything installed.
#
# Build-time tools (NOT shipped to users):
#   dylibbundler create-dmg librsvg   →  brew install dylibbundler create-dmg librsvg
# Everything else (iconutil, sips, install_name_tool, codesign, hdiutil) ships
# with macOS / the Xcode command-line tools.
#
# Usage:  packaging/macos/make-dmg.sh
# Output: dist/Soniq-<version>.dmg
set -euo pipefail

# ---- resolve paths --------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$ROOT"

APP_NAME="Soniq"
BIN_NAME="soniq"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
ICON_SVG="$SCRIPT_DIR/icon.svg"          # grey squircle + S, transparent corners
PLIST="$SCRIPT_DIR/Info.plist"

BUILD="$ROOT/target/macos-bundle"
APP="$BUILD/$APP_NAME.app"
DIST="$ROOT/dist"
DMG="$DIST/$APP_NAME-$VERSION.dmg"

echo "==> Soniq $VERSION → $DMG"

# ---- 1. release binary ----------------------------------------------------
echo "==> Building release binary"
cargo build --release
BIN="$ROOT/target/release/$BIN_NAME"
[ -x "$BIN" ] || { echo "!! missing $BIN"; exit 1; }

# ---- 2. .app skeleton -----------------------------------------------------
echo "==> Assembling $APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$APP/Contents/Frameworks"
cp "$BIN" "$APP/Contents/MacOS/$BIN_NAME"
cp "$PLIST" "$APP/Contents/Info.plist"

# ---- 3. icon (.icns) ------------------------------------------------------
echo "==> Rendering icon"
ICONSET="$BUILD/$APP_NAME.iconset"
rm -rf "$ICONSET"; mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  rsvg-convert -w "$s"        -h "$s"        "$ICON_SVG" -o "$ICONSET/icon_${s}x${s}.png"
  rsvg-convert -w "$((s*2))"  -h "$((s*2))"  "$ICON_SVG" -o "$ICONSET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/$APP_NAME.icns"

# ---- 4. bundle every non-system dylib (libmpv + transitive deps) ----------
echo "==> Bundling dynamic libraries (this is the no-Homebrew magic)"
dylibbundler \
  --overwrite-dir --bundle-deps \
  --fix-file "$APP/Contents/MacOS/$BIN_NAME" \
  --dest-dir "$APP/Contents/Frameworks/" \
  --install-path "@executable_path/../Frameworks/"

# ---- 4b. collapse duplicate LC_RPATH entries ------------------------------
# dylibbundler can leave two identical "@executable_path/../Frameworks/" rpaths
# in a dylib; modern dyld rejects duplicate LC_RPATHs ("Library not loaded …
# duplicate LC_RPATH"). Deps resolve via @executable_path directly, so collapse
# each to a single entry.
echo "==> De-duplicating rpaths"
RP="@executable_path/../Frameworks/"
count_rp() { otool -l "$1" | awk '/cmd LC_RPATH/{f=1} f&&/ path /{print $2; f=0}' | grep -cx "$RP" || true; }
for f in "$APP/Contents/MacOS/$BIN_NAME" "$APP/Contents/Frameworks/"*.dylib; do
  while [ "$(count_rp "$f")" -gt 1 ]; do
    install_name_tool -delete_rpath "$RP" "$f" 2>/dev/null || break
  done
done

# ---- 5. sanity: nothing should still point at /opt/homebrew or /usr/local --
echo "==> Verifying no Homebrew leakage"
if otool -L "$APP/Contents/MacOS/$BIN_NAME" "$APP/Contents/Frameworks/"*.dylib \
     | grep -E '/opt/homebrew|/usr/local|/Cellar' ; then
  echo "!! a library still references a Homebrew path — not self-contained"; exit 1
fi
echo "   clean: $(ls "$APP/Contents/Frameworks" | wc -l | tr -d ' ') libs bundled"

# ---- 6. ad-hoc code signature --------------------------------------------
# Signs the bundled dylibs first, then the app. Ad-hoc ("-") needs no Apple
# Developer account; it lets the app run on THIS Mac. See the note printed at
# the end about other machines / Gatekeeper.
echo "==> Ad-hoc code signing"
find "$APP/Contents/Frameworks" -name '*.dylib' -exec codesign --force --sign - {} +
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP" && echo "   signature OK"

# ---- 7. .dmg --------------------------------------------------------------
echo "==> Building DMG"
mkdir -p "$DIST"
rm -f "$DMG"
# Default to hdiutil (no GUI needed, never hangs). Set PRETTY=1 to use
# create-dmg for a custom window layout (drives Finder via AppleScript; only
# works in an interactive desktop session).
if [ "${PRETTY:-0}" = "1" ] && command -v create-dmg >/dev/null 2>&1; then
  # Pretty window with the app on the left and an Applications drop target.
  create-dmg \
    --volname "$APP_NAME $VERSION" \
    --window-size 600 360 \
    --icon-size 120 \
    --icon "$APP_NAME.app" 150 180 \
    --app-drop-link 450 180 \
    --hide-extension "$APP_NAME.app" \
    --no-internet-enable \
    "$DMG" "$APP" \
  || { echo "   create-dmg failed, falling back to hdiutil"; HDIUTIL_FALLBACK=1; }
else
  HDIUTIL_FALLBACK=1
fi

if [ "${HDIUTIL_FALLBACK:-0}" = "1" ]; then
  STAGE="$BUILD/dmg-stage"
  rm -rf "$STAGE"; mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname "$APP_NAME $VERSION" -srcfolder "$STAGE" \
    -ov -format UDZO "$DMG"
fi

echo
echo "==> Done:  $DMG"
echo "   Size:   $(du -h "$DMG" | cut -f1)"
echo
echo "   Install: open the .dmg, drag Soniq into Applications."
echo "   NOTE (other Macs): this build is ad-hoc signed, not notarized, so a"
echo "   downloaded copy is quarantined by Gatekeeper. On first launch the"
echo "   recipient must right-click the app → Open (once), or run:"
echo "       xattr -dr com.apple.quarantine /Applications/Soniq.app"
