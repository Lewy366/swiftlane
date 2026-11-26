# Working on Swiftlane

Swiftlane is a Windows toolkit app built in Rust (Tauri 2) in `Rust/` with HTML/CSS/JS UI in `Source/web`. These are the owner's standing rules for anyone (human or AI) working in this repo.

## Rules

- **Security first** (see `SECURITY.md`): pick the safest default, explain trade-offs in plain language, and never weaken a protection quietly.
- **Nothing goes out without the owner's OK.** That covers publishing releases, pushing and posting anything outward.
- **Work only on the `main` branch.**
- **Commits are in the owner's name:** `Lewy366 <196448197+lewy366@users.noreply.github.com>`, with no AI co-author lines.
- **Design work: research first.** Before changing how something looks or feels, look up current design ideas and guidelines online (for example Microsoft's Fluent design guidance and well-made apps that solve the same problem), then say briefly what was taken from them.
- **Release notes & README are users-only.** Write what features do, how to use them, and user-relevant security points — never internal housekeeping, file/crate/function names, test counts, CI, or build notes.
- **Explain things plainly and briefly.** Keep rounds concise and send screenshots as files.
- **Only build features the owner picks.**

## Architecture & Build

- **Main app (`Rust/`, `Swiftlane.exe`):** Tauri 2 + Axum/Tokio backend serving `Source/web` (offline Geist & Geist Mono fonts in `Source/web/fonts/`, Fluent UI System Icons). Handles Phone transfer + TLS/CA, Share & Request links (Tailscale Funnel), Send to contacts (`iroh`), Explorer right-click menu & authenticated loopback IPC (`explorer.rs`), PC remote, Screen mirroring (AirPlay via UxPlay, Miracast, browser WebRTC cast), Vault, PDF tools (`pdfium.dll`), Image editor, File converter, Video trimmer & Shrink to size & Downloader (`ffmpeg.exe`/`ffprobe.exe`/`yt-dlp.exe`), Storage & Cleanup (`diskmap.rs`), Find files (`findfiles.rs`), Duplicate finder (`duplicates.rs`), Startup check (`startup.rs`), BIOS update USB (`biosusb.rs`), QR Studio (`qrstudio.rs`), World clock, Network devices, Is my PC exposed?, Currency converter, Speed test, Ad blocker test, and Calculator helper (`Source/toolkit/calc.py`, the only runtime Python helper; `Source/make_icons.py` is dev-only).
- **Installer & Packer (`Rust/setup/`, `swiftlane-pack`):** Native Tauri 2 Setup wizard (`requireAdministrator` in release) + transactional rollback engine and `SWLSETUP` bundle packer (`Rust/pack/build-setup.ps1`), with thin LTO release builds (`lto = "thin"`, `codegen-units = 4`).
- **Releases:** Signed with an Ed25519 key that lives only on the owner's USB stick. The owner-only publisher (`Source/release/swiftlane-publish.exe` built from `Source/release/publish-rs/`, invoked by `Publish update.cmd`) stays off GitHub on purpose. A release can only be published from the owner's Windows PC with the stick plugged in.

## Checking Your Work

- **JavaScript:** `node --check Source/web/js/<file>.js`
- **Rust tests:** Inside `Rust/` run `cargo test --locked` (on local Windows without Tailscale/sandbox differences, `-- --skip share::tests::download_counting`; in `Rust/setup/` run `cargo test --locked`).
- **Tests must never create or delete files outside a fresh temp folder.**
- **CI (`.github/workflows/windows-rust.yml`):** verifies JS syntax, downloads & SHA-256-verifies pinned helpers (`ffmpeg.exe`/`ffprobe.exe`, `pdfium.dll`, `yt-dlp.exe`), runs `cargo test --locked`, builds release binaries, and packs the runnable folder.
