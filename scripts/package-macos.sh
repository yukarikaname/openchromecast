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

# Temp dirs created during packaging; removed on exit.
TMPDIRS=()
cleanup() { for d in "${TMPDIRS[@]}"; do rm -rf "$d"; done; }
trap cleanup EXIT

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
  <key>CFBundleIconFile</key>        <string>AppIcon</string>
  <key>CFBundleExecutable</key>      <string>openchromecast</string>
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>LSMinimumSystemVersion</key>  <string>10.15</string>
  <key>LSUIElement</key>             <true/>
  <key>NSHighResolutionCapable</key> <true/>
  <key>NSHumanReadableCopyright</key><string>MIT License</string>
  <!-- macOS 15 local-network privacy: without this the .app is silently
       blocked from mDNS, so Cast senders cannot discover it. -->
  <key>NSLocalNetworkUsageDescription</key>
  <string>OpenChromecast advertises itself on your local network so Cast senders (Android VLC, YouTube, ...) can discover and stream to it.</string>
  <key>NSBonjourServices</key>
  <array>
    <string>_googlecast._tcp</string>
  </array>
</dict>
</plist>
PLIST

# Generate the .app icon from the same cast-dot the tray draws, then bundle it
# as AppIcon.icns (must happen before signing so the code signature covers it).
ICON_DIR="$(mktemp -d)"
TMPDIRS+=("$ICON_DIR")
"$BIN" --dump-icon "$ICON_DIR/icon-1024.png"
mkdir -p "$ICON_DIR/AppIcon.iconset"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$ICON_DIR/icon-1024.png" --out "$ICON_DIR/AppIcon.iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z "$d" "$d" "$ICON_DIR/icon-1024.png" --out "$ICON_DIR/AppIcon.iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICON_DIR/AppIcon.iconset" -o "$APP/Contents/Resources/AppIcon.icns"
echo ">> app icon -> Contents/Resources/AppIcon.icns"

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
  TMPDIRS+=("$STAGE")
  ditto -x -k "$OUT" "$STAGE"
  if [[ -n "$NOTARY_KEY_BASE64" ]]; then
    echo "$NOTARY_KEY_BASE64" | base64 --decode > "$STAGE/AuthKey_$NOTARY_KEY_ID.p8"
    # Self-check the notary credentials so a 401 can be traced to one field.
    # (HTTP 401 from notarytool = the API-key auth was rejected.)
    ok=1
    if [[ -z "$NOTARY_KEY_ID" ]]; then
      echo "!! NOTARY_KEY_ID is empty" >&2; ok=0
    else
      echo "   NOTARY_KEY_ID: ${#NOTARY_KEY_ID} chars, matches .p8 name: $([[ -f "$STAGE/AuthKey_$NOTARY_KEY_ID.p8" ]] && echo yes || echo no)"
    fi
    if [[ -z "$NOTARY_ISSUER_ID" ]]; then
      echo "!! NOTARY_ISSUER_ID is empty" >&2; ok=0
    elif [[ ! "$NOTARY_ISSUER_ID" =~ ^[0-9A-Fa-f-]{36}$ ]]; then
      echo "!! NOTARY_ISSUER_ID does not look like a 36-char UUID: ${#NOTARY_ISSUER_ID} chars" >&2; ok=0
    else
      echo "   NOTARY_ISSUER_ID: 36-char UUID ok (ends ...${NOTARY_ISSUER_ID: -4})"
    fi
    if grep -q -- '-----BEGIN PRIVATE KEY-----' "$STAGE/AuthKey_$NOTARY_KEY_ID.p8"; then
      echo "   AuthKey .p8: valid PEM private key"
    else
      echo "!! AuthKey .p8 is NOT a valid PEM private key (base64 content wrong?)" >&2; ok=0
    fi
    [[ "$ok" -eq 0 ]] && { echo ">> aborting: fix the flagged notary credential(s) above" >&2; exit 1; }
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
