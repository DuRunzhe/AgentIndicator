#!/bin/bash
# 依据 Conventional Commits 与版本标签刷新 CHANGELOG.md。
#
# 发布前运行：它会以即将发布的版本作为标签名，把自上一个标签以来的提交归入
# 该版本这一段，再写入 CHANGELOG.md，随 “build: release” 提交一起入库。
#
# Usage:
#   bash scripts/update-changelog.sh            # 使用 Cargo.toml 里的当前版本
#   bash scripts/update-changelog.sh 0.2.31     # 指定即将发布的版本
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  VERSION=$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' Cargo.toml | head -1)
fi

if ! command -v git-cliff >/dev/null 2>&1; then
  echo "git-cliff not found; install it with 'brew install git-cliff' or 'cargo install git-cliff'" >&2
  exit 1
fi

git-cliff --config cliff.toml --tag "v$VERSION" -o CHANGELOG.md
echo "CHANGELOG.md refreshed (v$VERSION)"
