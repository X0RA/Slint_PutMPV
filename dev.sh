#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

export CARGO_INCREMENTAL=1
export RUSTFLAGS="${RUSTFLAGS:-}"

if command -v clang >/dev/null 2>&1 && command -v mold >/dev/null 2>&1; then
    export RUSTFLAGS="${RUSTFLAGS} -C linker=clang -C link-arg=-fuse-ld=mold"
elif command -v clang >/dev/null 2>&1 && command -v lld >/dev/null 2>&1; then
    export RUSTFLAGS="${RUSTFLAGS} -C linker=clang -C link-arg=-fuse-ld=lld"
fi

cargo run --profile dev-fast
