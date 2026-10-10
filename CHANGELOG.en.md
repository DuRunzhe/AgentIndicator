# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/).

This file is generated from the commit history by
[git-cliff](https://git-cliff.org). Do not edit it by hand.
The Chinese edition is [CHANGELOG.md](CHANGELOG.md).

## [0.2.30] - 2026-10-10

### Added

- Unify every conversation row as Kind (project) · title

### Fixed

- Gate the macOS-only dsh classification test to macOS
- List DeepSeek conversations only while something drives them

## [0.2.29] - 2026-10-09

### Fixed

- Recognize the npx dsh shim, and treat dsh web as profile-wide

## [0.2.29-alpha.2] - 2026-10-09

### Added

- Configure the hosted and DeepSeek ranges separately, name each form
- Report the terminal and desktop forms' absence separately
- Name the conversation a terminal row is driving
- One row per DeepSeek Harness conversation, navigating where it lives

### Fixed

- Measure a desktop conversation's duration from its turn, not its age
- Restore the ChatGPT range label and pin the two ranges as independent data
- Recognize the dsh CLI running under Node
- Recognize the dsh CLI by its package, not by one install shape
- Do not list sessions the runtime itself no longer serves
- Recognize the global dsh shim, so a live CLI is not reported as stopped
- List only the conversations the desktop application itself shows
- Do not list conversations the user archived
- List conversations whether or not the desktop application is running
- A conversation with no window open navigates to the CLI's web UI
- Detect a refused browser authorization, and drop the inverted fallback
- Restore the non-macOS build after the desktop-row refactor

## [0.2.29-alpha.1] - 2026-10-09

### Added

- Support the DeepSeek Harness desktop app
- Make a profile format change visible instead of silent

### Fixed

- Derive desktop state from the projection on every scan
- Detect a pending question as waiting for a reply
- Clear a decided approval as soon as the log moves
- Detect a failure from the retry record the log actually carries
- Report an offline failure instead of treating a closed turn as recovery
- An attempt that was started is not evidence of recovery
- Build on Windows and Linux (moved value in the sysinfo scan)

### Changed

- Derive log state from a timeline instead of matching event names

## [0.2.28] - 2026-09-26

### Added

- Foreground the owning terminal window on windows and linux

### Fixed

- Import AttachThreadInput from the threading module
- Treat the windows lock violation as a held instance lock
- Resolve Codex rollouts held by the app-server daemon

### Documentation

- Add windows and linux terminal focus verification steps

## [0.2.27-alpha.1] - 2026-09-21

### Fixed

- Rebind Pi sessions after /new
- Refresh Codex rollouts for long-lived host apps

## [0.2.24-alpha.2] - 2026-09-16

### Fixed

- Report failed Codex turns as errors

## [0.2.24-alpha.1] - 2026-09-13

### Documentation

- Align README with current implementation
- Restore v0.2.23 in README after the docs merge

### merge

- Align README with current implementation

## [0.2.23] - 2026-09-11

### Added

- Toggle the Claude collector from its menu row
- Add eleven more interface languages
- Make the follow-system language row self-explanatory

### Fixed

- Stabilize Codex terminal approval detection
- Reset Codex session state on turn abort

## [0.2.22] - 2026-09-10

### Added

- Check for updates from the About panel

## [0.2.21] - 2026-09-10

### Added

- List a host's active conversations as separate rows
- Let the user pick the ChatGPT conversation window

### Fixed

- Keep internal Codex threads out of the tray
- Show which ChatGPT conversation range is selected
- Do not let an interrupted tool call pin Pi to Working
- Report the application when no conversation is in range

## [0.2.20] - 2026-09-10

### Added

- Open the hosted session's own conversation on click

### Fixed

- Distinguish app-hosted agent sessions from standalone ones
- Recognize a host's pending permission request as waiting

## [0.2.19] - 2026-09-10

### Added

- Replace NSAlert with a custom padded panel (about)

### Fixed

- Resolve Pi context windows from the catalog store
- Use argument-less selectors for panel buttons (about)

## [0.2.18] - 2026-09-09

### Added

- Center the About panel content

## [0.2.17] - 2026-09-09

### Added

- Auto-repoint the Claude statusline on startup

### Fixed

- Brace APP_ASSET in install.sh so CJK locales do not abort it

## [0.2.16] - 2026-09-09

### Added

- Add an About dialog to the tray menu

### Fixed

- Keep a brand-new Pi session from inheriting a stale session's state
- Keep Claude awaiting-reply visible across the turn_duration marker

### Documentation

- Move install docs up and remove personal info from the repo
- Per-method 'add to applications' guidance, curl section first

## [0.2.15] - 2026-09-08

### Added

- Add a stopped-agent display toggle
- Ship a traffic-light app icon on macOS, Windows and Linux
- Install Linux desktop icons and a launcher entry

### Fixed

- Keep Pi sessions in the same directory independent
- Auto-resolve the latest version in install scripts

### Documentation

- Move installation and usage docs right after the intro

## [0.2.14] - 2026-09-07

### Fixed

- Prioritize Codex confirmation prompts
- React promptly to Codex approval review
- Retain structured Codex approval detection

### Documentation

- Split npm and Bun installs and document per-platform app installation
- Restructure install/usage docs with copy-friendly command blocks
- Drop the centralized upgrading section

## [0.2.13] - 2026-09-06

### Fixed

- Stop treating non-agent shells and errored Pi turns as working

### Documentation

- Add uninstall instructions for every channel

## [0.2.12] - 2026-09-06

### Documentation

- Rename implementation heading; ci: fix Windows zip packaging and Linux libxdo

## [0.2.11] - 2026-09-05

### Fixed

- Drop checkmark text prefixes from selectable menu rows
- Mark Pi working as soon as a user message starts a turn

### Documentation

- Describe the repository on its own terms
- Add English README with language switcher

## [0.2.10] - 2026-09-05

### Added

- Initial AgentStatusIndicator implementation
- Integrate Claude context statusline
- Add localized tray status UI
- Show last tray refresh time
- Add detector restart action
- Add native actionable notifications
- Load startup agent immediately
- Localize notification reminders
- Refresh system language automatically
- Add cross-platform startup entries
- Package macOS app bundle
- Use native macOS menu symbols
- Monitor Pi agent sessions
- Add notification type preferences

### Fixed

- Retain macOS process metadata on lsof races
- Prevent duplicate tray instances
- Request notification permission before enabling
- Bind macOS bundle identifier
- Present actionable notifications in foreground
- Sharpen native tray status icons
- Synchronize resumed agent sessions
- Localize dynamic menu labels
- Gray out notification preferences until notifications are enabled

### Changed

- Bound session analysis caches

### Documentation

- Clarify cross-platform startup support

