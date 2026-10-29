#!/usr/bin/env bash
# Assemble a macOS .app bundle around a built typst-editor binary.
#
# This script runs on macOS for signing/validation and on Linux/WSL (inside the
# osxcross cross build) for assembly. It never launches the app.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

usage() {
	cat <<'EOF'
Usage: scripts/macos/package-app.sh --binary <typst-editor> --out <Typst Editor.app> [options]

Options:
  --binary <path>    Mach-O executable to wrap (required)
  --out <path>       Output .app path; must be under <repo>/dist/macos/ or <repo>/dev-dist/macos/ and end in .app (required)
  --version <x.y.z>  Bundle version (default: the root Cargo.toml package version)
  --no-sign          Skip the ad-hoc codesign step on macOS
  -h, --help         Show this help

The bundle contains:
  Contents/Info.plist
  Contents/PkgInfo
  Contents/MacOS/typst-editor
  Contents/Resources/typ-edit.icns
EOF
}

BINARY=""
OUT=""
VERSION=""
SIGN=1
while [ "$#" -gt 0 ]; do
	case "$1" in
	--binary)
		BINARY="${2:?--binary needs a path}"
		shift 2
		;;
	--binary=*)
		BINARY="${1#*=}"
		shift
		;;
	--out)
		OUT="${2:?--out needs a path}"
		shift 2
		;;
	--out=*)
		OUT="${1#*=}"
		shift
		;;
	--version)
		VERSION="${2:?--version needs a value}"
		shift 2
		;;
	--version=*)
		VERSION="${1#*=}"
		shift
		;;
	--no-sign)
		SIGN=0
		shift
		;;
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
done

[ -n "$BINARY" ] || {
	echo "--binary is required" >&2
	exit 2
}
[ -n "$OUT" ] || {
	echo "--out is required" >&2
	exit 2
}
[ -f "$BINARY" ] || {
	echo "binary not found: $BINARY" >&2
	exit 1
}

case "$OUT" in
"$ROOT"/dist/macos/*.app | "$ROOT"/dev-dist/macos/*.app) ;;
*)
	echo "refusing to write outside $ROOT/dist/macos or $ROOT/dev-dist/macos: $OUT" >&2
	exit 2
	;;
esac

if [ -z "$VERSION" ]; then
	VERSION="$(awk -F'"' '/^version = "/ { print $2; exit }' "$ROOT/Cargo.toml")"
fi
case "$VERSION" in
[0-9]*.[0-9]*.[0-9]*) ;;
*)
	echo "invalid bundle version: $VERSION" >&2
	exit 1
	;;
esac

if [ -e "$OUT" ] && [ ! -d "$OUT/Contents" ]; then
	echo "refusing to replace a non-bundle path: $OUT" >&2
	exit 2
fi

rm -rf "$OUT"
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources"
install -m 0755 "$BINARY" "$OUT/Contents/MacOS/typst-editor"
sed "s/__VERSION__/$VERSION/g" "$ROOT/packaging/macos/Info.plist" >"$OUT/Contents/Info.plist"
cp "$ROOT/assets/app-icon/typ-edit-icon.icns" "$OUT/Contents/Resources/typ-edit.icns"
printf 'APPL????' >"$OUT/Contents/PkgInfo"

if [ "$(uname -s)" = "Darwin" ]; then
	plutil -lint "$OUT/Contents/Info.plist" >/dev/null
	if [ "$SIGN" = 1 ]; then
		# Ad-hoc sign the Mach-O first, then the bundle. Apple Silicon refuses to
		# execute unsigned arm64 binaries, and no developer certificate is assumed.
		codesign --force --sign - "$OUT/Contents/MacOS/typst-editor" >/dev/null
		codesign --force --sign - "$OUT" >/dev/null
		echo "ad-hoc signed: $OUT"
	fi
else
	echo "non-Darwin host: skipped plutil/codesign"
fi

if command -v file >/dev/null 2>&1; then
	file "$OUT/Contents/MacOS/typst-editor"
fi
printf '%s\n' "$OUT"
