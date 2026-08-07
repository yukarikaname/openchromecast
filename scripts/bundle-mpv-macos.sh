#!/usr/bin/env bash
#
# Build a self-contained portable mpv for macOS from a Homebrew install:
#   <out>/bin/mpv  +  <out>/lib/*.dylib
# Every non-system dylib is copied and re-linked against
# @executable_path/../lib so the .app needs no Homebrew at runtime.
#
# dylibbundler does the main work; a multi-pass otool/install_name_tool fixup
# catches any Homebrew absolute paths it missed, then the result is verified.
#
# Usage:
#   bash scripts/bundle-mpv-macos.sh [mpv-bin] [out-dir]
set -euo pipefail

MPV_BIN="${1:-$(command -v mpv)}"
OUT="${2:-"${RUNNER_TEMP:-/tmp}/mpv-portable"}"
LIBDIR="$OUT/lib"

command -v mpv >/dev/null 2>&1 || { echo "error: mpv not found" >&2; exit 1; }
command -v dylibbundler >/dev/null 2>&1 || brew install dylibbundler >/dev/null 2>&1 || true

rm -rf "$OUT"
mkdir -p "$OUT/bin" "$LIBDIR"
cp "$MPV_BIN" "$OUT/bin/mpv"
chmod +w "$OUT/bin/mpv"

# Primary bundler: copies dependencies and rewrites their install names to
# @executable_path/../lib.
if command -v dylibbundler >/dev/null 2>&1; then
  echo ">> dylibbundler..."
  dylibbundler -b -x "$OUT/bin/mpv" -d "$LIBDIR" -p @executable_path/../lib -cd -od \
    || echo "!! dylibbundler errored; proceeding with manual fixup"
fi

# Copy any remaining Homebrew dylib and rewrite the reference in $1 to
# @executable_path/../lib.
fixup() {
  local bin="$1" dep base
  while IFS= read -r dep; do
    base="$(basename "$dep")"
    if [[ ! -e "$LIBDIR/$base" && -e "$dep" ]]; then
      cp "$dep" "$LIBDIR/$base"
      chmod +w "$LIBDIR/$base"
      install_name_tool -id "@executable_path/../lib/$base" "$LIBDIR/$base" 2>/dev/null || true
    fi
    if [[ -e "$LIBDIR/$base" ]]; then
      install_name_tool -change "$dep" "@executable_path/../lib/$base" "$bin" 2>/dev/null || true
    fi
  done < <(otool -L "$bin" 2>/dev/null | awk 'NR>1{print $1}' | grep -E '^/(opt/homebrew|usr/local)/' || true)
}

echo ">> fixing remaining Homebrew absolute paths (multi-pass)..."
for pass in 1 2 3 4 5 6; do
  before="$(otool -L "$OUT/bin/mpv" "$LIBDIR"/*.dylib 2>/dev/null | grep -cE '^[[:space:]]+/(opt/homebrew|usr/local)/' || true)"
  fixup "$OUT/bin/mpv"
  for d in "$LIBDIR"/*.dylib; do
    [[ -e "$d" ]] && fixup "$d"
  done
  after="$(otool -L "$OUT/bin/mpv" "$LIBDIR"/*.dylib 2>/dev/null | grep -cE '^[[:space:]]+/(opt/homebrew|usr/local)/' || true)"
  echo "   pass $pass: absolute refs $before -> $after"
  [[ "$after" -eq 0 ]] && break
done

# install_name_tool invalidates ad-hoc signatures; arm64 macOS refuses to run
# binaries with broken signatures, so re-sign everything.
echo ">> re-signing (ad-hoc)..."
codesign --force --sign - "$OUT/bin/mpv" 2>/dev/null || true
for d in "$LIBDIR"/*.dylib; do
  [[ -e "$d" ]] && codesign --force --sign - "$d" 2>/dev/null || true
done

echo ">> final dependency list of bundled mpv:"
otool -L "$OUT/bin/mpv"

remaining="$(otool -L "$OUT/bin/mpv" "$LIBDIR"/*.dylib 2>/dev/null | grep -cE '^[[:space:]]+/(opt/homebrew|usr/local)/' || true)"
if [[ "$remaining" -gt 0 ]]; then
  echo "!! WARNING: $remaining Homebrew absolute refs remain; the .app is NOT fully self-contained." >&2
  exit 1
fi
echo ">> OK: mpv is self-contained (no Homebrew absolute refs)."
