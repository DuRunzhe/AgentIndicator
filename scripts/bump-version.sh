#!/bin/bash
# Bump the release version everywhere it is declared.
#
# Usage:
#   bash scripts/bump-version.sh 0.2.13          # real bump
#   bash scripts/bump-version.sh --dry-run 0.2.13  # preview only
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

DRY=0
if [[ "${1:-}" == "--dry-run" ]]; then
  DRY=1
  shift
fi
NEW="${1:?usage: bump-version.sh [--dry-run] <new-version>}"
[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
  || { echo "invalid version: $NEW (expected semver like 0.2.13)" >&2; exit 1; }

OLD=$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' Cargo.toml | head -1)
[ -n "$OLD" ] || { echo "cannot read current version from Cargo.toml" >&2; exit 1; }
[ "$OLD" = "$NEW" ] && { echo "already at $NEW"; exit 0; }

FILES=(Cargo.toml package.json scripts/package-macos-app.sh \
       Formula/agent-status-indicator.rb TESTING.md README.md README.en.md)

echo "bump $OLD -> $NEW in: ${FILES[*]}"
if [ "$DRY" = 1 ]; then
  echo "(--dry-run, nothing changed)"
  exit 0
fi

perl -pi -e "s/\Q$OLD\E/$NEW/g" "${FILES[@]}"
# The formula sha256 values belong to the previous release; reset them so a
# stale hash is never shipped. Fill them after CI with fill-release-shas.sh.
perl -pi -e 's/sha256 "[0-9a-f]{64}"/sha256 "REPLACE_ON_RELEASE"/g' Formula/agent-status-indicator.rb

echo "done."
echo "next: bash scripts/build-release.sh && cargo test --offline"
echo "      git add -A && git commit -m \"build: release $NEW\" && git push origin main"
echo "      git tag v$NEW && git push origin v$NEW"
echo "after CI finishes:"
echo "      bash scripts/fill-release-shas.sh v$NEW"
echo "      bash scripts/update-tap.sh          # commit+push tap formula"
