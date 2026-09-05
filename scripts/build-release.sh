#!/bin/bash
# Build the release binary while remapping the local home directory, so the
# published artifact never embeds the build machine's user path (e.g.
# /Users/<name>/.cargo/... from crate panic strings).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

home=$(cd ~ && pwd)
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${home}=~"

cargo build --release "$@"
