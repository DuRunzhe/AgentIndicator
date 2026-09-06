#!/bin/bash
# Commit and push the updated formula into the Homebrew tap repository.
#
# Usage:
#   bash scripts/update-tap.sh                       # auto-detect tap clone
#   TAP_DIR=/path/to/homebrew-tap bash scripts/update-tap.sh
#
# Detection order: TAP_DIR env, the tap installed by brew, then common clones.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)

TAP_DIR="${TAP_DIR:-}"
if [ -z "$TAP_DIR" ]; then
  BREW_TAP="$(brew --prefix 2>/dev/null)/Library/Taps/durunzhe/homebrew-tap"
  for c in "$BREW_TAP" "${ROOT}/../homebrew-tap" "$HOME/homebrew-tap" "/tmp/homebrew-tap"; do
    [ -d "$c/.git" ] && TAP_DIR="$c" && break
  done
fi
if [ -z "$TAP_DIR" ] || [ ! -d "$TAP_DIR/.git" ]; then
  echo "no tap repository found; set TAP_DIR=/path/to/homebrew-tap" >&2
  exit 1
fi

mkdir -p "$TAP_DIR/Formula"
cp "$ROOT/Formula/agent-status-indicator.rb" "$TAP_DIR/Formula/"

if git -C "$TAP_DIR" diff --quiet -- Formula/; then
  echo "formula unchanged ($TAP_DIR) — nothing to push"
  exit 0
fi

git -C "$TAP_DIR" add Formula/agent-status-indicator.rb
git -C "$TAP_DIR" -c user.name="DuRunzhe" -c user.email="durunzhe666@gmail.com" \
  commit -m "Update agent-status-indicator to v${VERSION}" >/dev/null
git -C "$TAP_DIR" push origin main >/dev/null 2>&1
echo "tap updated and pushed: $TAP_DIR (v${VERSION})"
