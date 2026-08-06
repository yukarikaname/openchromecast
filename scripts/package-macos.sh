#!/usr/bin/env bash
#
# Package the macOS release binary into a zipped, ad-hoc signed `.app` bundle.
#
# The `.app` is marked LSUIElement (menu-bar only, no dock icon) — appropriate
# for a tray/daemon app. Signing is ad-hoc (`codesign -s -`) so it runs on any
# Mac without a Developer ID; for notarized distribution you must supply an
# Apple Developer ID certificate (see the note below).
#
# Usage:
#   cargo build --release --target x86_64-apple-darwin   # (or host arch)
#   bash scripts/package-macos.sh [output.zip]
#
# Example:
#   bash scripts/package-macos.sh dist/OpenChromecast-macos.zip

set -euo pipefail

APP="OpenChromecast.app"
BIN="target/release/openchromecast"
OUT="${1:-dist/OpenChromecast-macos.zip}"
VERSION="${2:-1.0.0}"

if [[ ! -x "$BIN" ]]; then
  echo "error: binary not found: $BIN (run cargo build --release first)" >&2
  exit 1
fi

echo ">> building $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/openchromecast"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>            <string>OpenChromecast</string>
  <key>CFBundleDisplayName</key>     <string>OpenChromecast</string>
  <key>CFBundleIdentifier</key>      <string>io.openchromecast.app</string>
  <key>CFBundleVersion</key>         <string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key>      <string>openchromecast</string>
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>LSMinimumSystemVersion</key>  <string>10.15</string>
  <key>LSUIElement</key>             <true/>
  <key>NSHighResolutionCapable</key> <true/>
  <key>NSHumanReadableCopyright</key><string>MIT License</string>
</dict>
</plist>
PLIST

echo ">> ad-hoc codesign"
# `-s -` = ad-hoc signature. For public distribution, replace with:
#   codesign --force --deep --options runtime \
#     --sign "Developer ID Application: <Your Name> (TEAMID)" "$APP"
# and notarize with `xcrun notarytool submit ...`.
codesign --force --deep --sign - "$APP"

echo ">> zipping -> $OUT"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
ditto -c -k --keepParent "$APP" "$OUT"

echo ">> done: $OUT"
