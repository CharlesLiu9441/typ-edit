#!/usr/bin/env bash
# Download a macOS SDK from joseluisq/macosx-sdks, build osxcross, and write a
# sourceable environment file for the typ-edit cross build.
#
# osxcross only runs on Linux/BSD hosts. The SDK is not vendored: it is cached
# under the user cache directory and verified against the release checksum.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"

SDK_VERSION="${TYP_EDIT_MACOS_SDK_VERSION:-15.5}"
OSXCROSS_REF="${TYP_EDIT_OSXCROSS_REF:-27d21e4977c9751d01199c7a226a6faf494c3dd9}"
FLAVOR="${TYP_EDIT_OSXCROSS_FLAVOR:-llvm}"
DEPLOYMENT_TARGET="${TYP_EDIT_MACOS_DEPLOYMENT_TARGET:-11.0}"
CACHE="${TYP_EDIT_MACOS_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/typ-edit/macos-cross}"
ARCHS="arm64 x86_64"
FORCE=0
PRINT_ENV=0

DOWNLOADS="$CACHE/downloads"
OSXCROSS_DIR="$CACHE/osxcross"
OSXCROSS_SRC="$OSXCROSS_DIR/src"
OSXCROSS_TARGET="$OSXCROSS_DIR/target"
LOGS="$CACHE/logs"
ENV_FILE="$CACHE/env.sh"
MARKER="$OSXCROSS_TARGET/.typ-edit-toolchain"
SDK_NAME="MacOSX${SDK_VERSION}.sdk.tar.xz"
SDK_FILE="$DOWNLOADS/$SDK_NAME"
SUMS_FILE="$DOWNLOADS/sha256sum-${SDK_VERSION}.txt"
SDK_BASE="https://github.com/joseluisq/macosx-sdks/releases/download/${SDK_VERSION}"

usage() {
	cat <<'EOF'
Usage: scripts/macos/setup-osxcross.sh [options]

Downloads MacOSX<version>.sdk.tar.xz from
https://github.com/joseluisq/macosx-sdks, verifies it, builds the pinned
osxcross toolchain, and writes <cache>/env.sh for build-macos.sh.

Options:
  --ensure             Ensure the toolchain exists (default)
  --print-env          Print the sourceable environment file on stdout
  --force              Re-download the SDK and rebuild osxcross
  --sdk-version <v>    SDK release to use (default 15.5)
  -h, --help           Show this help

Environment overrides:
  TYP_EDIT_MACOS_CACHE                 cache root (default ~/.cache/typ-edit/macos-cross)
  TYP_EDIT_MACOS_SDK_VERSION           same as --sdk-version
  TYP_EDIT_OSXCROSS_REF                osxcross git commit (default pinned)
  TYP_EDIT_OSXCROSS_FLAVOR             llvm (default) or stable
  TYP_EDIT_MACOS_DEPLOYMENT_TARGET     default 11.0

Prerequisites (Debian/Ubuntu):
  sudo apt-get install -y clang llvm lld cmake git make patch sed tar \
    gzip xz-utils bzip2 cpio
EOF
}

die() {
	echo "error: $*" >&2
	exit 1
}

find_tool() {
	local name dir
	for name in "$@"; do
		if command -v "$name" >/dev/null 2>&1; then
			command -v "$name"
			return 0
		fi
		for dir in /usr/lib/llvm-*/bin; do
			if [ -x "$dir/$name" ]; then
				printf '%s\n' "$dir/$name"
				return 0
			fi
		done
	done
	return 1
}

sha256_file() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | awk '{ print $1 }'
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | awk '{ print $1 }'
	else
		die "neither sha256sum nor shasum is available"
	fi
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--ensure) ;;
	--print-env) PRINT_ENV=1 ;;
	--force) FORCE=1 ;;
	--sdk-version)
		SDK_VERSION="${2:?--sdk-version needs a value}"
		shift
		;;
	--sdk-version=*) SDK_VERSION="${1#*=}" ;;
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

case "$(uname -s)" in
Darwin)
	echo "macOS host: osxcross is not needed; run scripts/macos/build-macos.sh directly." >&2
	exit 0
	;;
Linux | *BSD) ;;
MINGW* | MSYS* | CYGWIN*)
	die "osxcross requires a Linux or BSD host. Open this repository in WSL2 (Ubuntu) and run the script there; see docs/macos.md."
	;;
*)
	die "unsupported host: $(uname -s); osxcross supports Linux and BSD only"
	;;
esac

LLVM_CONFIG="$(find_tool llvm-config llvm-config-21 llvm-config-20 llvm-config-19 llvm-config-18 llvm-config-17 || true)"
LLVM_AR="$(find_tool llvm-ar llvm-ar-21 llvm-ar-20 llvm-ar-19 llvm-ar-18 llvm-ar-17 || true)"
LLVM_RANLIB="$(find_tool llvm-ranlib llvm-ranlib-21 llvm-ranlib-20 llvm-ranlib-19 llvm-ranlib-18 llvm-ranlib-17 || true)"
LLVM_LIPO="$(find_tool llvm-lipo llvm-lipo-21 llvm-lipo-20 llvm-lipo-19 llvm-lipo-18 llvm-lipo-17 || true)"
LD64_LLD="$(find_tool ld64.lld || true)"

write_env() {
	local arm_clang x86_clang arm_name x86_name sdkroot ar_tool ranlib_tool lipo_tool

	# osxcross also ships *-cmake-clang launchers; the compiler is the plain
	# *-clang wrapper.
	arm_clang="$(find "$OSXCROSS_TARGET/bin" -maxdepth 1 -name 'arm64-apple-darwin*-clang' ! -name '*-cmake-*' -print | sort | head -n1)"
	if [ -z "$arm_clang" ]; then
		arm_clang="$(find "$OSXCROSS_TARGET/bin" -maxdepth 1 -name 'aarch64-apple-darwin*-clang' ! -name '*-cmake-*' -print | sort | head -n1)"
	fi
	x86_clang="$(find "$OSXCROSS_TARGET/bin" -maxdepth 1 -name 'x86_64-apple-darwin*-clang' ! -name '*-cmake-*' -print | sort | head -n1)"
	[ -n "$arm_clang" ] || die "osxcross is missing the arm64 *-clang wrapper; re-run with --force"
	[ -n "$x86_clang" ] || die "osxcross is missing the x86_64 *-clang wrapper; re-run with --force"

	arm_name="$(basename "$arm_clang")"
	x86_name="$(basename "$x86_clang")"
	sdkroot="$OSXCROSS_TARGET/SDK/MacOSX${SDK_VERSION}.sdk"
	[ -d "$sdkroot" ] || die "osxcross is missing $sdkroot; re-run with --force"

	ar_tool="${LLVM_AR:-$(find_tool llvm-ar || true)}"
	ranlib_tool="${LLVM_RANLIB:-$(find_tool llvm-ranlib || true)}"
	lipo_tool="${LLVM_LIPO:-$(find_tool llvm-lipo || true)}"
	[ -n "$ar_tool" ] || die "llvm-ar is required to write env.sh"
	[ -n "$ranlib_tool" ] || die "llvm-ranlib is required to write env.sh"
	[ -n "$lipo_tool" ] || die "llvm-lipo is required to write env.sh"

	{
		echo "# Generated by scripts/macos/setup-osxcross.sh; source this file."
		printf 'export TYP_EDIT_OSXCROSS_TARGET=%q\n' "$OSXCROSS_TARGET"
		printf 'export OSXCROSS_BIN=%q\n' "$OSXCROSS_TARGET/bin"
		printf 'export OSXCROSS_LIPO=%q\n' "$lipo_tool"
		printf 'export SDKROOT=%q\n' "$sdkroot"
		printf 'export MACOSX_DEPLOYMENT_TARGET=%q\n' "$DEPLOYMENT_TARGET"
		printf 'export PATH=%q:"$PATH"\n' "$OSXCROSS_TARGET/bin"
		printf 'export CC_aarch64_apple_darwin=%q\n' "$arm_name"
		printf 'export CXX_aarch64_apple_darwin=%q\n' "${arm_name}++"
		printf 'export AR_aarch64_apple_darwin=%q\n' "$ar_tool"
		printf 'export RANLIB_aarch64_apple_darwin=%q\n' "$ranlib_tool"
		printf 'export CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=%q\n' "$arm_name"
		printf 'export CC_x86_64_apple_darwin=%q\n' "$x86_name"
		printf 'export CXX_x86_64_apple_darwin=%q\n' "${x86_name}++"
		printf 'export AR_x86_64_apple_darwin=%q\n' "$ar_tool"
		printf 'export RANLIB_x86_64_apple_darwin=%q\n' "$ranlib_tool"
		printf 'export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER=%q\n' "$x86_name"
	} >"$ENV_FILE"
}

MARKER_OK=0
if [ "$FORCE" = 0 ] && [ -f "$MARKER" ] && [ -d "$OSXCROSS_TARGET/bin" ]; then
	if grep -qx "sdk=$SDK_VERSION" "$MARKER" &&
		grep -qx "osxcross_ref=$OSXCROSS_REF" "$MARKER" &&
		grep -qx "flavor=$FLAVOR" "$MARKER" &&
		grep -qx "deployment=$DEPLOYMENT_TARGET" "$MARKER" &&
		find "$OSXCROSS_TARGET/bin" -maxdepth 1 -name '*-apple-darwin*-clang' ! -name '*-cmake-*' -print -quit | grep -q .; then
		MARKER_OK=1
	fi
fi

if [ "$MARKER_OK" = 1 ]; then
	echo "osxcross cache is up to date: $OSXCROSS_TARGET" >&2
	write_env
	if [ "$PRINT_ENV" = 1 ]; then
		cat "$ENV_FILE"
	else
		echo "source \"$ENV_FILE\"" >&2
	fi
	exit 0
fi

missing=()
require() {
	if ! command -v "$1" >/dev/null 2>&1; then
		missing+=("$1")
	fi
}

require clang
require clang++
require cmake
require make
require git
require curl
require xz
require patch
require sed
require tar
require gzip
require bzip2
require cpio

if [ "$FLAVOR" = "llvm" ]; then
	[ -n "$LLVM_CONFIG" ] || missing+=("llvm-config")
	[ -n "$LLVM_AR" ] || missing+=("llvm-ar")
	[ -n "$LLVM_RANLIB" ] || missing+=("llvm-ranlib")
	[ -n "$LLVM_LIPO" ] || missing+=("llvm-lipo")
	[ -n "$LD64_LLD" ] || missing+=("ld64.lld")
fi

if [ "${#missing[@]}" -gt 0 ]; then
	echo "missing required tools: ${missing[*]}" >&2
	cat >&2 <<'EOF'
Install them first (Debian/Ubuntu):
  sudo apt-get update
  sudo apt-get install -y clang llvm lld cmake git make patch sed tar \
    gzip xz-utils bzip2 cpio
Then re-run this script.
EOF
	exit 2
fi

mkdir -p "$DOWNLOADS" "$OSXCROSS_DIR" "$LOGS"

download() {
	local url="$1"
	local dest="$2"
	if [ "$FORCE" = 0 ] && [ -f "$dest" ]; then
		return 0
	fi
	echo "downloading $url" >&2
	curl -fL --retry 3 --retry-delay 2 --connect-timeout 20 -o "$dest.part" "$url"
	mv "$dest.part" "$dest"
}

echo "The macOS SDK is Apple software; downloading it means you accept the Xcode license:" >&2
echo "  https://www.apple.com/legal/sla/docs/xcode.pdf" >&2
echo "SDK: $SDK_BASE/$SDK_NAME" >&2

download "$SDK_BASE/$SDK_NAME" "$SDK_FILE"
download "$SDK_BASE/sha256sum.txt" "$SUMS_FILE"

expected="$(awk -v want="$SDK_NAME" '{ name=$NF; sub(/^\*/, "", name); if (name == want) { print $1; exit } }' "$SUMS_FILE")"
[ -n "$expected" ] || die "no checksum for $SDK_NAME in $SUMS_FILE"
actual="$(sha256_file "$SDK_FILE")"
[ "$expected" = "$actual" ] || die "SDK checksum mismatch for $SDK_FILE (expected $expected, got $actual)"

case "$SDK_VERSION" in
15.5) pinned="c15cf0f3f17d714d1aa5a642da8e118db53d79429eb015771ba816aa7c6c1cbd" ;;
26.1) pinned="beee7212d265a6d2867d0236cc069314b38d5fb3486a6515734e76fa210c784c" ;;
*) pinned="" ;;
esac
if [ -n "$pinned" ]; then
	[ "$pinned" = "$actual" ] || die "SDK checksum does not match the pinned digest for $SDK_VERSION"
fi

if [ ! -d "$OSXCROSS_SRC/.git" ]; then
	mkdir -p "$OSXCROSS_SRC"
	git -C "$OSXCROSS_SRC" init -q
	git -C "$OSXCROSS_SRC" remote add origin https://github.com/tpoechtrager/osxcross.git
fi
if ! git -C "$OSXCROSS_SRC" fetch --depth 1 origin "$OSXCROSS_REF"; then
	echo "shallow fetch by ref failed; fetching the full history" >&2
	git -C "$OSXCROSS_SRC" fetch origin
fi
git -C "$OSXCROSS_SRC" checkout -q --detach FETCH_HEAD
resolved_ref="$(git -C "$OSXCROSS_SRC" rev-parse HEAD)"
echo "osxcross: $resolved_ref" >&2

mkdir -p "$OSXCROSS_SRC/tarballs"
ln -sfn "$SDK_FILE" "$OSXCROSS_SRC/tarballs/$SDK_NAME"

build_log="$LOGS/osxcross-build-${SDK_VERSION}-${resolved_ref:0:12}.log"
echo "building osxcross ($FLAVOR) ... log: $build_log" >&2
set +e
(
	cd "$OSXCROSS_SRC"
	UNATTENDED=1 \
		BUILD_FLAVOR="$FLAVOR" \
		ENABLE_ARCHS="$ARCHS" \
		TARGET_DIR="$OSXCROSS_TARGET" \
		OSX_VERSION_MIN="$DEPLOYMENT_TARGET" \
		./build.sh </dev/null
) 2>&1 | tee "$build_log"
status=${PIPESTATUS[0]}
set -e
[ "$status" -eq 0 ] || die "osxcross build failed (exit $status); see $build_log"

write_env

{
	echo "sdk=$SDK_VERSION"
	echo "osxcross_ref=$OSXCROSS_REF"
	echo "resolved_ref=$resolved_ref"
	echo "flavor=$FLAVOR"
	echo "deployment=$DEPLOYMENT_TARGET"
	echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$MARKER"

echo "osxcross ready: $OSXCROSS_TARGET" >&2
if [ "$PRINT_ENV" = 1 ]; then
	cat "$ENV_FILE"
else
	echo "source \"$ENV_FILE\"" >&2
fi
