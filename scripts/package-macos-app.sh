#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SOURCE="${1:-$ROOT/target/release/agent-status-indicator}"
APP="$ROOT/app/darwin-arm64/AgentStatusIndicator.app"
CONTENTS="$APP/Contents"

if [[ ! -x "$SOURCE" ]]; then
  echo "release binary not found: $SOURCE" >&2
  exit 1
fi

mkdir -p "$CONTENTS/MacOS"
cp "$SOURCE" "$CONTENTS/MacOS/AgentStatusIndicator"
cat > "$CONTENTS/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>AgentStatusIndicator</string>
  <key>CFBundleExecutable</key><string>AgentStatusIndicator</string>
  <key>CFBundleIdentifier</key><string>com.durunzhe.agent-status-indicator</string>
  <key>CFBundleName</key><string>AgentStatusIndicator</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.2.13</string>
  <key>CFBundleVersion</key><string>0.2.13</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST

# Sign the bundle.  An explicit Developer ID identity enables hardened-runtime
# signing that can be notarized; without one the app stays ad-hoc signed so
# local development does not require a certificate.
IDENTITY="${ASI_SIGN_IDENTITY:-}"
if [[ -n "$IDENTITY" ]]; then
  codesign --force --options runtime --timestamp \
    --entitlements "$ROOT/app/AgentStatusIndicator.entitlements" \
    --sign "$IDENTITY" "$CONTENTS/MacOS/AgentStatusIndicator"
  codesign --force --options runtime --timestamp \
    --sign "$IDENTITY" "$APP"
else
  codesign --force --sign - "$APP"
fi

echo "$APP"
