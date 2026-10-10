#!/bin/bash
# 依据 Conventional Commits 与版本标签刷新各语言的 CHANGELOG 文件。
#
# 发布前运行：它会以即将发布的版本作为标签名，把自上一个标签以来的提交归入
# 该版本这一段，再写入文件，随 “build: release” 提交一起入库。
#
# 语言由配置与输出文件成对给定：每加一种语言就加一个 cliff.<lang>.toml 和
# 一行对应关系。
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

# 配置:输出，新增语言时在末尾追加一行。
LANGUAGES=(
  "cliff.toml:CHANGELOG.md"
  "cliff.en.toml:CHANGELOG.en.md"
)

for entry in "${LANGUAGES[@]}"; do
  config="${entry%%:*}"
  output="${entry##*:}"
  git-cliff --config "$config" --tag "v$VERSION" -o "$output"
  echo "$output refreshed (v$VERSION)"
done
