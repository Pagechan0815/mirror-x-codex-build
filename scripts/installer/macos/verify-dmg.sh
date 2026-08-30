#!/usr/bin/env bash
set -euo pipefail

DMG="${1:?usage: verify-dmg.sh <dmg> <x64|arm64>}"
ARCH="${2:?usage: verify-dmg.sh <dmg> <x64|arm64>}"

case "$ARCH" in
  x64) MACH_ARCH="x86_64" ;;
  arm64) MACH_ARCH="arm64" ;;
  *) echo "error: unsupported architecture: $ARCH" >&2; exit 2 ;;
esac

test -f "$DMG"
hdiutil verify "$DMG"

MOUNT_POINT="$(mktemp -d "${TMPDIR:-/tmp}/mirror-x-codex-dmg.XXXXXX")"
cleanup() {
  hdiutil detach "$MOUNT_POINT" -force >/dev/null 2>&1 || true
  rmdir "$MOUNT_POINT" >/dev/null 2>&1 || true
}
trap cleanup EXIT

hdiutil attach "$DMG" -readonly -nobrowse -mountpoint "$MOUNT_POINT" >/dev/null
test -L "$MOUNT_POINT/Applications"
test "$(readlink "$MOUNT_POINT/Applications")" = "/Applications"

for app in "$MOUNT_POINT/mirror x codex.app" "$MOUNT_POINT/mirror x codex 管理器.app"; do
  plist="$app/Contents/Info.plist"
  executable_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")"
  executable="$app/Contents/MacOS/$executable_name"

  plutil -lint "$plist" >/dev/null
  test -f "$app/Contents/PkgInfo"
  test "$(cat "$app/Contents/PkgInfo")" = "APPL????"
  test -f "$app/Contents/Resources/mirror-x-codex.icns"
  test -x "$executable"
  test "$(stat -f '%z' "$executable")" -ge 1024
  test "$(head -c 2 "$executable")" != '#!'
  codesign --verify --deep --strict "$app"
  file "$executable" | grep -q "$MACH_ARCH"
  otool -L "$executable" >/dev/null
done

SILENT_PLIST="$MOUNT_POINT/mirror x codex.app/Contents/Info.plist"
MANAGER_PLIST="$MOUNT_POINT/mirror x codex 管理器.app/Contents/Info.plist"
test "$(/usr/libexec/PlistBuddy -c 'Print :LSUIElement' "$SILENT_PLIST")" = "true"
test "$(/usr/libexec/PlistBuddy -c 'Print :LSUIElement' "$MANAGER_PLIST")" = "false"
MANAGER_URL_SCHEMES="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleURLTypes:0:CFBundleURLSchemes' "$MANAGER_PLIST")"
printf '%s\n' "$MANAGER_URL_SCHEMES" | grep -q 'mirrorplus'
printf '%s\n' "$MANAGER_URL_SCHEMES" | grep -q 'codexplusplus'

MANAGER="$MOUNT_POINT/mirror x codex 管理器.app/Contents/MacOS/mirror-x-codex-manager"
IMAGEGEN="$MOUNT_POINT/mirror x codex 管理器.app/Contents/MacOS/mirror-x-imagegen"
test -x "$IMAGEGEN"
codesign --verify --strict "$IMAGEGEN"
file "$IMAGEGEN" | grep -q "$MACH_ARCH"
otool -L "$IMAGEGEN" >/dev/null
IMAGEGEN_HELP="$("$IMAGEGEN" 2>&1 || true)"
printf '%s' "$IMAGEGEN_HELP" | grep -q "mirror-x-imagegen generate"

"$MANAGER" >"${TMPDIR:-/tmp}/mirror-x-codex-manager-smoke.log" 2>&1 &
MANAGER_PID=$!
sleep 5
if ! kill -0 "$MANAGER_PID" 2>/dev/null; then
  cat "${TMPDIR:-/tmp}/mirror-x-codex-manager-smoke.log" >&2 || true
  echo "error: manager exited during macOS smoke test" >&2
  exit 1
fi
kill "$MANAGER_PID"
wait "$MANAGER_PID" 2>/dev/null || true

echo "verified DMG and manager startup: $DMG ($MACH_ARCH)"
