#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=native"

cargo build --release
exec ./target/release/putmpv
