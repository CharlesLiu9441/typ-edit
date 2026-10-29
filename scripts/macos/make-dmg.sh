#!/usr/bin/env bash
# Build a styled macOS DMG (app + Applications symlink + Finder background/positions).
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
CACHE="${TYP_EDIT_MACOS_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/typ-edit/macos-cross}"

APP=""
OUT=""
BACKGROUND="$ROOT/packaging/macos/dmg-background.png"
VOLUME_ICON="$ROOT/assets/app-icon/typ-edit-icon.icns"
BACKEND="auto"
VOLUME_NAME="Typst Editor"
GO_BIN="${TYP_EDIT_GO:-}"

usage() {
	cat <<'EOF'
Usage: scripts/macos/make-dmg.sh --app <Typst Editor.app> --out <Typst Editor.dmg> [options]

Creates a styled DMG with:
  - the .app bundle
  - an Applications -> /Applications symlink
  - a Finder background image, window geometry, and icon positions
  - a volume icon

Options:
  --app <path>          .app bundle to include (required)
  --out <path>          output .dmg path (required)
  --background <path>   Finder background image (default packaging/macos/dmg-background.png)
  --volume-icon <path>  volume icon (.icns/.png/.jpg; default assets/app-icon/typ-edit-icon.icns)
  --volume-name <name>  mounted volume name (default "Typst Editor")
  --backend <name>      auto (default), native, or hdiutil
  -h, --help            show this help

Requires Go 1.24+ to build scripts/macos/dmg-helper. On Linux/WSL the helper
uses the pure-Go HFS+/UDIF backend; on macOS "auto" uses hdiutil.
EOF
}

die() {
	echo "error: $*" >&2
	exit 1
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--app)
		APP="${2:?--app needs a path}"
		shift
		;;
	--app=*) APP="${1#*=}" ;;
	--out)
		OUT="${2:?--out needs a path}"
		shift
		;;
	--out=*) OUT="${1#*=}" ;;
	--background)
		BACKGROUND="${2:?--background needs a path}"
		shift
		;;
	--background=*) BACKGROUND="${1#*=}" ;;
	--volume-icon)
		VOLUME_ICON="${2:?--volume-icon needs a path}"
		shift
		;;
	--volume-icon=*) VOLUME_ICON="${1#*=}" ;;
	--volume-name)
		VOLUME_NAME="${2:?--volume-name needs a value}"
		shift
		;;
	--volume-name=*) VOLUME_NAME="${1#*=}" ;;
	--backend)
		BACKEND="${2:?--backend needs a value}"
		shift
		;;
	--backend=*) BACKEND="${1#*=}" ;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	esac
	shift
done

[ -n "$APP" ] || die "--app is required"
[ -n "$OUT" ] || die "--out is required"
[ -d "$APP" ] || die "app bundle not found: $APP"
[ -f "$BACKGROUND" ] || die "background not found: $BACKGROUND"
[ -f "$VOLUME_ICON" ] || die "volume icon not found: $VOLUME_ICON"

case "$BACKEND" in
auto | native | hdiutil) ;;
*) die "unsupported backend: $BACKEND (use auto, native, or hdiutil)" ;;
esac

if [ -z "$GO_BIN" ]; then
	GO_BIN="$(command -v go || true)"
fi
[ -n "$GO_BIN" ] || die "Go 1.24+ is required to build the DMG helper; install it from https://go.dev/dl/"

goversion="$("$GO_BIN" env GOVERSION 2>/dev/null || "$GO_BIN" version | awk '{print $3}')"
goversion="${goversion#go}"
case "$goversion" in
[0-9]*.[0-9]*) ;;
*) die "cannot parse Go version: $goversion" ;;
esac
major="${goversion%%.*}"
rest="${goversion#*.}"
minor="${rest%%.*}"
if [ "$major" -lt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -lt 24 ]; }; then
	die "Go 1.24+ is required (found $goversion); install it from https://go.dev/dl/"
fi

mkdir -p "$(dirname "$OUT")" "$CACHE/bin"
(
	cd "$ROOT/scripts/macos/dmg-helper"
	GOTOOLCHAIN=local "$GO_BIN" build -trimpath -o "$CACHE/bin/typ-edit-mkdmg" .
)

exec "$CACHE/bin/typ-edit-mkdmg" \
	-app "$APP" \
	-out "$OUT" \
	-background "$BACKGROUND" \
	-volume-icon "$VOLUME_ICON" \
	-volume-name "$VOLUME_NAME" \
	-backend "$BACKEND"
