#!/bin/sh
# Install agent-status-indicator on macOS / Linux from a GitHub Release.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
#   # or pin a version:
#   VERSION=0.2.10 curl -fsSL https://raw.githubusercontent.com/DuRunzhe/AgentIndicator/main/scripts/install.sh | sh
#   # install elsewhere:
#   PREFIX=/usr/local/bin curl -fsSL ... | sh   (root)
set -eu

REPO="DuRunzhe/AgentIndicator"
VERSION="${VERSION:-}"

# Resolve the newest release automatically. VERSION is optional and only pins
# an older build; each source below is tried in turn so a blocked or
# rate-limited endpoint never forces the caller to specify a version.
resolve_latest_version() {
  # 1) GitHub REST API (authoritative, but anonymous calls are rate-limited)
  version=$(curl -fsSL --max-time 15 "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
  if [ -n "$version" ]; then
    printf '%s' "${version#v}"
    return
  fi
  # 2) releases/latest Location redirect, which needs no API quota
  location=$(curl -fsS --max-time 15 -o /dev/null -w '%{redirect_url}' "https://github.com/$REPO/releases/latest" 2>/dev/null || true)
  case "$location" in
    */releases/tag/*)
      version=${location##*/releases/tag/}
      [ -n "$version" ] && { printf '%s' "${version#v}"; return; }
      ;;
  esac
  # 3) npm registry, which carries the same tagged release version
  version=$(curl -fsSL --max-time 15 "https://registry.npmjs.org/agent-status-indicator/latest" 2>/dev/null \
    | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
  [ -n "$version" ] && { printf '%s' "$version"; return; }
  :
}

# Linux: drop the hicolor icons into ~/.local/share and register a desktop
# entry so the app shows up in launchers with the traffic-light artwork.
install_linux_desktop() {
  prefix=$1
  data_root="${XDG_DATA_HOME:-$HOME/.local/share}"
  icon_root="$data_root/icons/hicolor"
  mkdir -p "$icon_root" "$data_root/applications" || return 0
  for size in 16 32 48 64 128 256 512; do
    directory="$icon_root/${size}x${size}/apps"
    mkdir -p "$directory"
    if ! curl -fsSL --max-time 20 \
      "https://raw.githubusercontent.com/$REPO/main/icons/hicolor/${size}x${size}/apps/agent-status-indicator.png" \
      -o "$directory/agent-status-indicator.png"; then
      echo "警告: 下载 ${size}px 图标失败，跳过" >&2
    fi
  done
  cat > "$data_root/applications/agent-status-indicator.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=AgentStatusIndicator
Comment=Native tray monitor for AI coding agents
Exec="$prefix/agent-status-indicator"
Icon=agent-status-indicator
Terminal=false
Categories=Utility;
EOF
  echo "已创建 Linux 桌面入口：$data_root/applications/agent-status-indicator.desktop"
}

if [ -z "$VERSION" ] || [ "$VERSION" = "latest" ]; then
  VERSION=$(resolve_latest_version)
fi
if [ -z "$VERSION" ]; then
  echo "无法自动获取最新版本：GitHub API、releases 页面与 npm registry 均不可达。" >&2
  echo "请检查网络后重试，或临时用 VERSION=0.2.14 显式指定版本安装。" >&2
  exit 1
fi

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS-$ARCH" in
  Darwin-arm64)             TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64|Darwin-amd64) TARGET=x86_64-apple-darwin ;;
  Linux-x86_64|Linux-amd64) TARGET=x86_64-unknown-linux-gnu ;;
  Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
  *) echo "暂不支持的平台: $OS-$ARCH" >&2; exit 1 ;;
esac

BASE="https://github.com/$REPO/releases/download/v$VERSION"
ASSET="agent-status-indicator-$TARGET.tar.gz"
PREFIX="${PREFIX:-$HOME/.local/bin}"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "下载 ${ASSET} (v${VERSION}) ..."
curl -fL --retry 3 --retry-delay 2 -sS "$BASE/$ASSET" -o "$TMP/$ASSET" || {
  echo "该平台/版本制品暂未发布（${TARGET} @ v${VERSION}）" >&2
  echo "请查看 https://github.com/${REPO}/releases 确认可用版本" >&2
  exit 1
}

# Verify against the bare-hex .sha256 sidecar published next to the asset.
if curl -fsSL --retry 2 "$BASE/$ASSET.sha256" -o "$TMP/$ASSET.sha256" 2>/dev/null; then
  if command -v shasum >/dev/null 2>&1; then
    ACTUAL=$(shasum -a 256 "$TMP/$ASSET" | awk '{print $1}')
  else
    ACTUAL=$(sha256sum "$TMP/$ASSET" | awk '{print $1}')
  fi
  EXPECTED=$(tr -d '[:space:]' < "$TMP/$ASSET.sha256")
  [ "$ACTUAL" = "$EXPECTED" ] || { echo "SHA256 校验失败" >&2; exit 1; }
  echo "SHA256 校验通过"
else
  echo "警告: 未找到 .sha256 校验文件，跳过校验" >&2
fi

mkdir -p "$PREFIX"
tar -xzf "$TMP/$ASSET" -C "$PREFIX" agent-status-indicator
chmod +x "$PREFIX/agent-status-indicator"

if [ "$OS" = "Linux" ]; then
  install_linux_desktop "$PREFIX"
fi

# macOS: also install the notarized .app bundle, so the tray app shows up in
# Launchpad / Spotlight and can be started with `open -a AgentStatusIndicator`.
# Releases publish it as <target>.app.tar.gz (currently Apple Silicon only);
# when the asset is absent (e.g. x86_64 macOS) we keep the CLI binary alone.
if [ "$OS" = "Darwin" ]; then
  APP_ASSET="agent-status-indicator-$TARGET.app.tar.gz"
  echo "下载 ${APP_ASSET} (v${VERSION}) ..."
  if curl -fL --retry 3 --retry-delay 2 -sS "$BASE/$APP_ASSET" -o "$TMP/$APP_ASSET" 2>/dev/null; then
    if curl -fsSL --retry 2 "$BASE/$APP_ASSET.sha256" -o "$TMP/$APP_ASSET.sha256" 2>/dev/null; then
      if command -v shasum >/dev/null 2>&1; then
        ACTUAL=$(shasum -a 256 "$TMP/$APP_ASSET" | awk '{print $1}')
      else
        ACTUAL=$(sha256sum "$TMP/$APP_ASSET" | awk '{print $1}')
      fi
      EXPECTED=$(tr -d '[:space:]' < "$TMP/$APP_ASSET.sha256")
      [ "$ACTUAL" = "$EXPECTED" ] || { echo "SHA256 校验失败（$APP_ASSET）" >&2; exit 1; }
      echo "SHA256 校验通过（$APP_ASSET）"
    else
      echo "警告: 未找到 $APP_ASSET.sha256 校验文件，跳过校验" >&2
    fi
    tar -xzf "$TMP/$APP_ASSET" -C "$TMP"
    # /Applications is admin-writable on most Macs; otherwise fall back to the
    # user-level ~/Applications.
    APP_DIR=""
    for candidate in /Applications "$HOME/Applications"; do
      if mkdir -p "$candidate" 2>/dev/null && [ -w "$candidate" ]; then
        rm -rf "$candidate/AgentStatusIndicator.app"
        cp -R "$TMP/AgentStatusIndicator.app" "$candidate/"
        APP_DIR="$candidate"
        break
      fi
    done
    if [ -n "$APP_DIR" ]; then
      echo "已安装 .app：${APP_DIR}/AgentStatusIndicator.app"
      echo "可用 open -a AgentStatusIndicator 或从启动台（Launchpad）启动"
    else
      echo "警告: 无法写入 /Applications 或 ~/Applications，未能安装 .app" >&2
      echo "可改为手动安装，或改用 npm / Bun 方式安装（同样含公证 .app）" >&2
    fi
  else
    echo "注意: 该版本未随 Release 发布 ${TARGET} 的 .app（x86_64 macOS 等暂无），仅安装命令行二进制" >&2
  fi
fi

echo "已安装: ${PREFIX}/agent-status-indicator (v${VERSION})"
case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *) echo "请将 ${PREFIX} 加入 PATH：export PATH=\"${PREFIX}:\$PATH\"" ;;
esac
echo "运行 agent-status-indicator --diagnose 查看状态；不带参数启动托盘"
