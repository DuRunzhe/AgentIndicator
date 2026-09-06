#!/bin/bash
# Developer ID sign + notarize + staple the macOS .app bundle.
#
# Prerequisites (one-time):
#   xcrun notarytool store-credentials "AC_API_KEY" \
#     --key-id <KEY_ID> --issuer-id <ISSUER_ID> --key <AuthKey_XXXX.p8>
#   (or "AC_PASSWORD" via --apple-id/--team-id/--password)
#
# Usage:
#   ASI_SIGN_IDENTITY="Developer ID Application: Runzhe Du (W796BPAJVP)" \
#   ASI_TEAM_ID="W796BPAJVP" \
#   ASI_NOTARY_PROFILE="AC_API_KEY" \
#   scripts/notarize-macos-app.sh
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
APP="$ROOT/app/darwin-arm64/AgentStatusIndicator.app"
IDENTITY="${ASI_SIGN_IDENTITY:?set ASI_SIGN_IDENTITY to the Developer ID identity}"
PROFILE="${ASI_NOTARY_PROFILE:-}"

# 1) Repackage and sign with the Developer ID identity (hardened runtime).
ASI_SIGN_IDENTITY="$IDENTITY" "$ROOT/scripts/package-macos-app.sh"

# 2) Zip keeping the parent folder, as notarytool requires.
STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT
ZIP="$STAGING/AgentStatusIndicator.zip"
ditto -c -k --keepParent "$APP" "$ZIP"

# 3) Submit and wait for Apple's verdict (typically 1-5 minutes). Prefer
# direct API-key credentials (no keychain prompts on headless CI); fall back
# to a stored keychain profile for local runs.
if [[ -n "${ASI_NOTARY_KEY_FILE:-}" ]]; then
  xcrun notarytool submit "$ZIP" \
    --key "$ASI_NOTARY_KEY_FILE" \
    --key-id "${ASI_NOTARY_KEY_ID:?set ASI_NOTARY_KEY_ID}" \
    --issuer "${ASI_NOTARY_ISSUER_ID:?set ASI_NOTARY_ISSUER_ID}" \
    --wait
else
  xcrun notarytool submit "$ZIP" \
    --keychain-profile "${PROFILE:?set ASI_NOTARY_PROFILE or ASI_NOTARY_KEY_FILE}" \
    --team-id "${ASI_TEAM_ID:?set ASI_TEAM_ID when using a keychain profile}" \
    --wait
fi

# 4) Staple the ticket into the bundle and validate it.
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"

# 5) Sanity checks: signature chain and Gatekeeper assessment.
codesign --verify --deep --strict --verbose=2 "$APP"
spctl -a -vv --type execute "$APP"

echo "notarized and stapled: $APP"
