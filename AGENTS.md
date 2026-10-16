# AGENTS.md

Project-specific rules for AI coding agents. Architecture: `README.md`. Manual test flows: `TESTING.md`.

## Commands

```bash
cargo test                          # required; CI runs `cargo test --locked` on macOS, Linux and Windows
cargo fmt                           # keep the tree rustfmt-clean
bash scripts/build-release.sh       # release build (remaps the local path)
cargo run --release                 # run the tray
cargo run --release -- --diagnose   # probe without the tray
bash scripts/package-macos-app.sh   # build the .app (ad-hoc signed unless ASI_SIGN_IDENTITY is set)
```

## Platforms

- Code must compile on macOS, Linux and Windows. Gate OS-specific code with `#[cfg(target_os = "...")]`, declare per-target deps in `Cargo.toml`, and keep macOS-only tests behind `#[cfg(target_os = "macos")]`. A change verified only on the current machine is not verified.

## i18n

- User-facing text lives in `src/i18n.rs` and must exist in all 14 locales: `en`, `zh-Hans`, `zh-Hant`, `ja`, `ko`, `id`, `ms`, `vi`, `es`, `fr`, `it`, `de`, `ru`, `tr`.
- Adding or removing a key also requires updating the `TEXT_KEYS` / `MENU_KEYS` arrays (and their lengths) in the i18n tests.
- Tests fail if a locale is empty or a translated locale falls back to Simplified Chinese; use `SHARED_WITH_CHINESE` only for genuine word collisions.

## Commits

- Conventional Commits with an English subject.
- Commit messages are copied verbatim into `CHANGELOG*.md`: write them for end users, not developers.
- Types that reach the changelog: `feat`, `fix`, `docs`, `perf`, `refactor`, `revert`. Everything else is skipped, including `build: release ...`, `chore: fill the Homebrew sha256 ...`, `chore(release) ...`, and all other `chore` / `build` / `ci` / `test` / `style`.
- Never hand-edit `CHANGELOG.md` or `CHANGELOG.en.md`; `git-cliff` generates them.

## Versioning and release

- The version is declared in `Cargo.toml` (source for the About panel), `package.json`, `scripts/package-macos-app.sh`, `Formula/agent-status-indicator.rb`, `README*.md` and `TESTING.md`. Bump all of them with `bash scripts/bump-version.sh <version>`.
- Channel follows parity: an odd version is a stable release, an even version is an alpha (e.g. `0.2.31` stable, `0.2.32-alpha.N` alpha).
- Pushing a `v<version>` tag triggers the release workflow. Do not push tags unless explicitly asked.

## Do not commit

- `target/`, `dist/`, `app/darwin-arm64/`, `.claude/` (see `.gitignore`).
