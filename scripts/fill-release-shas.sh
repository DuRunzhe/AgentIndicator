#!/bin/bash
# Fill the per-platform sha256 values in Formula/agent-status-indicator.rb with
# the hashes published by CI for the given tag. The formula lives in this
# repository, which is also the Homebrew tap, so there is nowhere else to copy
# it: commit the change with the release.
#
# Four assets are covered: macOS and Linux, each on arm64 (aarch64) and x86_64.
# Every one of them must be published before this script succeeds, otherwise the
# formula would ship a hash that does not match its url.
#
# Usage:
#   bash scripts/fill-release-shas.sh v0.2.13
#   REPO=DuRunzhe/AgentIndicator bash scripts/fill-release-shas.sh v0.2.13
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

TAG="${1:-}"
if [ -z "$TAG" ]; then
  V=$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' Cargo.toml | head -1)
  TAG="v$V"
fi
TAG=${TAG#v}
REPO="${REPO:-DuRunzhe/AgentIndicator}"
BASE="https://github.com/$REPO/releases/download/v$TAG"
FORMULA="$ROOT/Formula/agent-status-indicator.rb"

fetch_sha() {
  local asset="agent-status-indicator-$1.tar.gz.sha256" sha tmp
  sha="$(curl -fsSL --retry 4 --retry-all-errors "$BASE/$asset" 2>/dev/null | tr -d '[:space:]')"
  if [[ -z "$sha" ]]; then
    # The releases/download gateway occasionally answers 504; the API route
    # used by the gh CLI serves the same asset.
    tmp="$(mktemp -d)"
    gh release download "v$TAG" --repo "$REPO" --pattern "$asset" --dir "$tmp" --clobber \
      >/dev/null 2>&1 || true
    if [[ -f "$tmp/$asset" ]]; then
      sha="$(tr -d '[:space:]' < "$tmp/$asset")"
    fi
    rm -rf "$tmp"
  fi
  printf '%s' "$sha"
  [[ -n "$sha" ]]
}

ARM_MAC=$(fetch_sha aarch64-apple-darwin) \
  || { echo "cannot fetch aarch64-apple-darwin sha (release v$TAG not ready?)" >&2; exit 1; }
X64_MAC=$(fetch_sha x86_64-apple-darwin) \
  || { echo "cannot fetch x86_64-apple-darwin sha (release v$TAG not ready?)" >&2; exit 1; }
X64_LINUX=$(fetch_sha x86_64-unknown-linux-gnu) \
  || { echo "cannot fetch x86_64-unknown-linux-gnu sha (release v$TAG not ready?)" >&2; exit 1; }
ARM_LINUX=$(fetch_sha aarch64-unknown-linux-gnu) \
  || { echo "cannot fetch aarch64-unknown-linux-gnu sha (release v$TAG has no ARM64 Linux asset yet)" >&2; exit 1; }

echo "v$TAG  mac arm64   = $ARM_MAC"
echo "v$TAG  mac x86_64  = $X64_MAC"
echo "v$TAG  linux arm64 = $ARM_LINUX"
echo "v$TAG  linux x64   = $X64_LINUX"

python3 - "$FORMULA" "$ARM_MAC" "$X64_MAC" "$X64_LINUX" "$ARM_LINUX" <<'PY'
import re, sys
path, arm_mac, x64_mac, x64_linux, arm_linux = sys.argv[1:6]
s = open(path).read()

def replace_in_block(s, asset, value):
    # anchor to the asset URL so each sha lands on its own block
    idx = s.index(asset)
    head, tail = s[:idx], s[idx:]
    tail = re.sub(
        r'sha256 "(?:REPLACE_ON_RELEASE|[0-9a-f]{64})"',
        'sha256 "%s"' % value, tail, count=1,
    )
    return head + tail

s = replace_in_block(s, 'agent-status-indicator-aarch64-apple-darwin.tar.gz', arm_mac)
s = replace_in_block(s, 'agent-status-indicator-x86_64-apple-darwin.tar.gz', x64_mac)
s = replace_in_block(s, 'agent-status-indicator-x86_64-unknown-linux-gnu.tar.gz', x64_linux)
s = replace_in_block(s, 'agent-status-indicator-aarch64-unknown-linux-gnu.tar.gz', arm_linux)
open(path, 'w').write(s)
print("updated", path)
PY
echo "Formula/agent-status-indicator.rb updated; commit and push it with the release"
