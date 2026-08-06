#!/usr/bin/env bash
#
# Package the macOS release binary into a zipped, signed `.app` bundle.
#
# The `.app` is marked LSUIElement (menu-bar only, no dock icon) — appropriate
# for a tray/daemon app.
#
# Signing (release only):
#   * SIGN_IDENTITY empty       -> ad-hoc (`codesign -s -`), used for local/dev.
#   * SIGN_IDENTITY set         -> Developer ID signing with hardened runtime,
#     e.g. SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)".
#     This is what the release workflow sets, so signing happens only in CI.
#
# Notarization (optional; requires SIGN_IDENTITY). Provide either an
# App Store Connect API key:
#     NOTARY_KEY_BASE64=...  NOTARY_KEY_ID=...  NOTARY_ISSUER_ID=...
# or an Apple ID + app-specific password:
#     APPLE_ID=...  APPLE_TEAM_ID=...  APPLE_PASSWORD=...
#
# Usage:
#   cargo build --release --target aarch64-apple-darwin
#   bash scripts/package-macos.sh [output.zip]
#
# Example:
#   bash scripts/package-macos.sh dist/OpenChromecast-macos-arm64.zip

set -euo pipefail

APP="OpenChromecast.app"
# Apple Silicon release build; override with BIN=<path> if needed.
BIN="${BIN:-target/aarch64-apple-darwin/release/openchromecast}"
OUT="${1:-dist/OpenChromecast-macos-arm64.zip}"
VERSION="${2:-1.0.0}"

# Developer ID codesign identity; empty => ad-hoc signing.
SIGN_IDENTITY="${SIGN_IDENTITY:-}"
# Notarization credentials (used only when SIGN_IDENTITY is set).
NOTARY_KEY_BASE64="${NOTARY_KEY_BASE64:-}"
NOTARY_KEY_ID="${NOTARY_KEY_ID:-}"
NOTARY_ISSUER_ID="${NOTARY_ISSUER_ID:-}"
APPLE_ID="${APPLE_ID:-}"
APPLE_TEAM_ID="${APPLE_TEAM_ID:-}"
APPLE_PASSWORD="${APPLE_PASSWORD:-}"

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

if [[ -n "$SIGN_IDENTITY" ]]; then
  echo ">> signing with '$SIGN_IDENTITY' (hardened runtime)"
  codesign --force --options runtime --sign "$SIGN_IDENTITY" "$APP"
else
  echo ">> ad-hoc codesign (no SIGN_IDENTITY)"
  codesign --force --sign - "$APP"
fi

echo ">> zipping -> $OUT"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"
ditto -c -k --keepParent "$APP" "$OUT"

if [[ -n "$SIGN_IDENTITY" && ( -n "$NOTARY_KEY_BASE64" || -n "$APPLE_ID" ) ]]; then
  echo ">> notarizing..."
  STAGE="$(mktemp -d)"
  trap 'rm -rf "$STAGE"' EXIT
  ditto -x -k "$OUT" "$STAGE"
  if [[ -n "$NOTARY_KEY_BASE64" ]]; then
    echo "$NOTARY_KEY_BASE64" | base64 --decode > "$STAGE/AuthKey_$NOTARY_KEY_ID.p8"
    xcrun notarytool submit "$OUT" \
      --key "$STAGE/AuthKey_$NOTARY_KEY_ID.p8" \
      --key-id "$NOTARY_KEY_ID" \
      --issuer "$NOTARY_ISSUER_ID" \
      --wait
  else
    xcrun notarytool submit "$OUT" \
      --apple-id "$APPLE_ID" \
      --team-id "$APPLE_TEAM_ID" \
      --password "$APPLE_PASSWORD" \
      --wait
  fi
  echo ">> stapling notarization ticket"
  xcrun stapler staple "$STAGE/$APP"
  ditto -c -k --keepParent "$STAGE/$APP" "$OUT"
else
  echo ">> not notarizing (set SIGN_IDENTITY plus notary credentials to enable)"
fi

echo ">> done: $OUT"
