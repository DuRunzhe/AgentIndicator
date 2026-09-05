# winget (Windows Package Manager) submission scaffold.
#
# How to publish to the community repo:
#   1. Build the Windows x64 zip via the release workflow (v0.2.11+), then run:
#        wingetcreate update DuRunzhe.AgentStatusIndicator --urls <zip-url> --version 0.2.11
#      (or `wingetcreate new` for the first submission). wingetcreate computes
#      the SHA256 and writes the manifests below.
#   2. Open a pull request against microsoft/winget-pkgs with these manifests.
#   3. Once merged: winget install --id DuRunzhe.AgentStatusIndicator
#
# The Windows zip asset is produced by .github/workflows/release.yml
# (target x86_64-pc-windows-msvc) and must contain agent-status-indicator.exe
# at the archive root.
