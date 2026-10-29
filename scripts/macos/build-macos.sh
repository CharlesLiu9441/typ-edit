#!/usr/bin/env bash
# Build typst-editor for macOS, natively on macOS or through osxcross on Linux/WSL.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
CACHE="${TYP_EDIT_MACOS_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/typ-edit/macos-cross}"
SDK_VERSION="${TYP_EDIT_MACOS_SDK_VERSION:-15.5}"
DIST=""
ARCH="aarch64"
PROFILE="release"
MODE="build"
PACKAGE=1
DMG=1
CLEAN=0

usage() {
	cat <<'EOF'
Usage: scripts/macos/build-macos.sh [options]

Builds the typst-editor binary for macOS. On macOS this uses the installed
Xcode Command Line Tools; on Linux/WSL it ensures osxcross (downloading the
macOS SDK from joseluisq/macosx-sdks) and then cross-compiles.

Options:
  --arch <aarch64|x86_64|universal>  target architecture (default aarch64)
  --profile <dev|release>            cargo profile (default release)
  --check                            run cargo check instead of a full build
  --package                          assemble the profile dist .app (default)
  --no-package                       leave the raw binary only
  --dmg                              create the profile dist .dmg (default with --package)
  --no-dmg                           skip the styled DMG
  --sdk-version <v>                  SDK release for the cross toolchain (default 15.5)
  --clean                            remove the selected profile's output directory before building
  -h, --help                         show this help

Release output: dist/macos; dev output: dev-dist/macos.

Environment:
  TYP_EDIT_MACOS_CACHE          toolchain/cargo cache
  TYP_EDIT_MACOS_SDK_VERSION    same as --sdk-version
  CARGO_TARGET_DIR              build output directory (default: repo target,
                                or the cache under /mnt/* for WSL speed)

DMG generation requires Go 1.24+; use --no-dmg to skip it.

Windows users: run this inside WSL2 (Ubuntu), not from Git Bash/PowerShell.
EOF
}

die() {
	echo "error: $*" >&2
	exit 1
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--arch)
		ARCH="${2:?--arch needs a value}"
		shift
		;;
	--arch=*) ARCH="${1#*=}" ;;
	--profile)
		PROFILE="${2:?--profile needs a value}"
		shift
		;;
	--profile=*) PROFILE="${1#*=}" ;;
	--check) MODE="check" ;;
	--package) PACKAGE=1 ;;
	--no-package) PACKAGE=0 ;;
	--dmg) DMG=1 ;;
	--no-dmg) DMG=0 ;;
	--sdk-version)
		SDK_VERSION="${2:?--sdk-version needs a value}"
		shift
		;;
	--sdk-version=*) SDK_VERSION="${1#*=}" ;;
	--clean)
		CLEAN=1
		shift
		continue
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
	shift
done

case "$ARCH" in
arm64) ARCH=aarch64 ;;
intel) ARCH=x86_64 ;;
aarch64 | x86_64 | universal) ;;
*)
	die "unsupported --arch: $ARCH (use aarch64, x86_64, or universal)"
	;;
esac

case "$PROFILE" in
dev | release) ;;
*) die "unsupported --profile: $PROFILE (use dev or release)" ;;
esac

if [ "$PROFILE" = "release" ]; then
	if [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
		die "CARGO_ENCODED_RUSTFLAGS takes precedence over RUSTFLAGS; unset it for release builds"
	fi
	export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build --remap-path-prefix=$PWD=/src"
	DIST="$ROOT/dist/macos"
else
	DIST="$ROOT/dev-dist/macos"
fi
if [ "$CLEAN" = 1 ]; then
	rm -rf "$DIST"
fi

case "$ARCH" in
aarch64) TRIPLES=(aarch64-apple-darwin) ;;
x86_64) TRIPLES=(x86_64-apple-darwin) ;;
universal) TRIPLES=(aarch64-apple-darwin x86_64-apple-darwin) ;;
esac

case "$(uname -s)" in
Darwin) NATIVE=1 ;;
Linux | *BSD) NATIVE=0 ;;
MINGW* | MSYS* | CYGWIN*)
	die "Windows shells cannot build macOS binaries directly. Install WSL2 (Ubuntu), open this repo under /mnt/<drive>/..., and run: bash scripts/macos/build-macos.sh --arch aarch64 --package"
	;;
*)
	die "unsupported host: $(uname -s)"
	;;
esac

# Non-login shells (CI, wsl.exe) do not source ~/.cargo/env.
if [ -d "$HOME/.cargo/bin" ]; then
	export PATH="$PATH:$HOME/.cargo/bin"
fi

command -v cargo >/dev/null 2>&1 || die "cargo was not found; install Rust with rustup first"
command -v rustup >/dev/null 2>&1 || die "rustup was not found; this script uses 'rustup target add'"

if [ "$NATIVE" = 0 ]; then
	bash "$SCRIPT_DIR/setup-osxcross.sh" --ensure --sdk-version "$SDK_VERSION"
	# shellcheck disable=SC1090
	source "$CACHE/env.sh"
	if [[ "$ROOT" == /mnt/* ]] && [ -z "${CARGO_TARGET_DIR:-}" ]; then
		export CARGO_TARGET_DIR="$CACHE/cargo-target"
		echo "WSL path detected; using CARGO_TARGET_DIR=$CARGO_TARGET_DIR" >&2
	fi
fi

for triple in "${TRIPLES[@]}"; do
	rustup target add "$triple"
done

if [ "$PROFILE" = "dev" ]; then
	OUT_PROFILE="debug"
else
	OUT_PROFILE="release"
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$DIST" "$CACHE/logs"
VERSION="$(awk -F'"' '/^version = "/ { print $2; exit }' "$ROOT/Cargo.toml")"
[ -n "$VERSION" ] || die "cannot read the package version from Cargo.toml"

build_one() {
	local triple="$1"
	local log="$CACHE/logs/cargo-${MODE}-${triple}-${PROFILE}.log"
	echo "cargo $MODE --target $triple (log: $log)" >&2
	set +e
	if [ "$MODE" = "check" ]; then
		cargo check --target "$triple" --bin typst-editor 2>&1 | tee "$log"
	else
		cargo build --profile "$PROFILE" --target "$triple" --bin typst-editor 2>&1 | tee "$log"
	fi
	local status=${PIPESTATUS[0]}
	set -e
	[ "$status" -eq 0 ] || die "cargo $MODE failed for $triple; see $log"
}

for triple in "${TRIPLES[@]}"; do
	build_one "$triple"
done

if [ "$MODE" = "check" ]; then
	echo "check passed for ${TRIPLES[*]}"
	exit 0
fi

BIN="$TARGET_DIR/${TRIPLES[0]}/$OUT_PROFILE/typst-editor"
[ -f "$BIN" ] || die "expected binary not found: $BIN"

if [ "$ARCH" = "universal" ]; then
	universal="$DIST/typst-editor-universal"
	lipo="${OSXCROSS_LIPO:-}"
	if [ -z "$lipo" ]; then
		for candidate in llvm-lipo osxcross-cctools-lipo lipo; do
			if command -v "$candidate" >/dev/null 2>&1; then
				lipo="$(command -v "$candidate")"
				break
			fi
		done
	fi
	[ -n "$lipo" ] || die "no lipo tool found for the universal build"
	x86="$TARGET_DIR/x86_64-apple-darwin/$OUT_PROFILE/typst-editor"
	[ -f "$x86" ] || die "expected x86_64 binary not found: $x86"
	"$lipo" -create "$BIN" "$x86" -output "$universal"
	BIN="$universal"
fi

if command -v file >/dev/null 2>&1; then
	file "$BIN"
fi

if [ "$PACKAGE" = 1 ]; then
	app="$DIST/Typst Editor.app"
	bash "$SCRIPT_DIR/package-app.sh" --binary "$BIN" --out "$app" --version "$VERSION"
	archive="$DIST/Typst Editor.app.tar.gz"
	tar -czf "$archive" -C "$DIST" "Typst Editor.app"
	echo "archive: $archive"
	echo "artifact: $app"
	if [ "$DMG" = 1 ]; then
		dmg="$DIST/Typst Editor.dmg"
		if ! bash "$SCRIPT_DIR/make-dmg.sh" --app "$app" --out "$dmg" \
			--background "$ROOT/packaging/macos/dmg-background.png" \
			--volume-icon "$ROOT/assets/app-icon/typ-edit-icon.icns"; then
			echo "error: DMG generation failed; the .app and .tar.gz were kept (use --no-dmg to skip the DMG)." >&2
			exit 1
		fi
		echo "dmg artifact: $dmg"
	fi
else
	echo "artifact: $BIN"
fi
