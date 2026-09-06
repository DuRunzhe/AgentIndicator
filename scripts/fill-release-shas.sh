#!/bin/bash
# Fill the two macOS sha256 values in Formula/agent-status-indicator.rb with
# the hashes published by CI for the given tag.
#
# Usage:
#   bash scripts/fill-release-shas.sh v0.2.13          # repo formula only
#   TAP_DIR=/path/to/homebrew-tap bash scripts/fill-release-shas.sh v0.2.13
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
  curl -fsSL --retry 4 --retry-all-errors "$BASE/agent-status-indicator-$1.tar.gz.sha256" \
    | tr -d '[:space:]'
}

ARM=$(fetch_sha aarch64-apple-darwin) \
  || { echo "cannot fetch aarch64 sha (release v$TAG not ready?)" >&2; exit 1; }
X64=$(fetch_sha x86_64-apple-darwin) \
  || { echo "cannot fetch x86_64 sha (release v$TAG not ready?)" >&2; exit 1; }
echo "v$TAG  aarch64=$ARM"
echo "v$TAG  x86_64=$X64"

python3 - "$FORMULA" "$ARM" "$X64" <<'PY'
import re, sys
path, arm, x64 = sys.argv[1:4]
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

s = replace_in_block(s, 'agent-status-indicator-aarch64-apple-darwin.tar.gz', arm)
s = replace_in_block(s, 'agent-status-indicator-x86_64-apple-darwin.tar.gz', x64)
open(path, 'w').write(s)
print("updated", path)
PY

if [[ -n "${TAP_DIR:-}" ]]; then
  mkdir -p "$TAP_DIR/Formula"
  cp "$FORMULA" "$TAP_DIR/Formula/"
  echo "copied formula to $TAP_DIR/Formula/ (commit & push it there)"
else
  echo "repo formula updated. If the tap lives elsewhere, run again with"
  echo "TAP_DIR=/path/to/homebrew-tap or copy Formula/agent-status-indicator.rb"
fi
