#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

if [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
	echo "error: CARGO_ENCODED_RUSTFLAGS takes precedence over RUSTFLAGS; unset it for release builds" >&2
	exit 2
fi

export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build --remap-path-prefix=$PWD=/src"
exec cargo build --release "$@"
