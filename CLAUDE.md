# Working on Swiftlane

Swiftlane is a Windows toolkit app (Python + pywebview, HTML/CSS/JS in `Source/web`). These are the owner's standing rules for anyone (human or AI) working in this repo.

## Rules

- **Security first** (see SECURITY.md): pick the safest default, explain trade-offs in plain language, and never weaken a protection quietly.
- **Nothing goes out without the owner's OK.** That covers publishing releases, pushing and posting anything outward.
- **Work only on the `main` branch.**
- **Commits are in the owner's name:** `Lewy366 <196448197+lewy366@users.noreply.github.com>`, with no AI co-author lines.
- **Design work: research first.** Before changing how something looks or feels, look up current design ideas and guidelines online (for example Microsoft's Fluent design guidance and well-made apps that solve the same problem), then say briefly what was taken from them.
- **Explain things plainly and briefly.** The owner keeps an eye on usage limits, so keep rounds short and send screenshots as files.
- **Only build features the owner picks.**

## Releases

- Releases are signed with a key that lives only on the owner's USB stick. The release tools (`Publish update.cmd`, `Source/release/`) are kept off GitHub on purpose.
- A release can only be published from the owner's Windows PC with the stick plugged in. Cloud sessions prepare and push code only, then say that a release still has to be published from the PC.
- Release notes go in CHANGELOG.md and the GitHub release (the publish script writes both). Changes to README text alone don't get a changelog entry.

## Cloud sessions (Linux)

They can't run the app window, Windows APIs, Tailscale or the two-PC tests. Check JavaScript with `node --check`, keep Python importable, and don't claim window or network behaviour was verified.

## Rust version (in progress)

The owner decided to rebuild Swiftlane in Rust (Tauri) and stop releasing the Python version; the calculator stays a Python helper. The Rust app lives in `Rust/`, shows the same interface (`Source/web`, via `js/bridge.js`) and keeps the same settings files. GitHub builds and tests it on Windows (`.github/workflows/windows-rust.yml`); Linux can `cargo test` locally.

- Done: window and settings, background picture, start with Windows, updater (same signing key), phone transfer incl. encrypted connections (reuses the Python CA), share links incl. request links, previews and Tailscale Funnel.
- Next: the tools (see `Source/toolkit`), PC remote and casting, then setup/uninstall/Explorer menu and the release pipeline.
- Tests must never create or delete files outside a fresh temp folder.
