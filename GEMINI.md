# Notes for Gemini (and any other AI picking up the work)

Read `CLAUDE.md` first. Its rules are the owner's rules and apply to you too (security first,
`main` branch only, commits in the owner's name with no AI co-author lines, nothing published
without the owner's OK, plain and short explanations).

## What's going on

Swiftlane is being rebuilt from Python into Rust (Tauri 2). The Rust app is in `Rust/`. It shows
the same web page as the Python app (`Source/web`). `Source/web/js/bridge.js` turns the page's
`window.pywebview.api.X(...)` calls into Rust calls, so **the page doesn't change**: Rust only has
to answer the same method names with the same JSON as the Python version did.

The Python version (`Source/swiftlane.py`, `Source/toolkit/*.py`, `Source/*.py`) is the
**reference**: when porting something, match what Python does, including its messages, file
names and settings keys. Don't edit the Python files. The only part that stays Python is the
calculator (`Source/toolkit/calc.py`), run as a helper program.

## How the Rust app is laid out

- `main.rs`: starts the window, background threads, drag & drop.
- `api.rs`: `call` gets every page call and tries `sharing::dispatch`, then `tools::dispatch`,
  then its own methods. `AppState` holds the settings and shared state. `push(app, event, data)`
  sends events to the page (`window.onCore`).
- `config.rs`: the same settings files as Python (`%APPDATA%\Swiftlane\config.json` and so on).
- `jobs.rs`: long tasks with progress, cancel and a "Done" notification. Use it for anything
  that takes longer than a moment (`Jobs::start(app, kind, title, notify, |ctx| ...)`, with
  `ctx.report(...)` for progress and `ctx.check()?` for cancel).
- `tools/mod.rs`: **where each tool plugs in.** A tool is one file `tools/<name>.rs` with
  `pub fn dispatch(app, method, args) -> Option<Value>` (return `None` for methods it doesn't
  know). Add it to the list in `tools::dispatch`. Shared helpers there: `arg`, `ask_save`,
  `ask_folder`, `program("ffmpeg")` (finds helper programs next to the exe, never in the current
  folder), `command()` (no console window), `save_dir`.
- Good examples to copy the style from: `tools/diskmap.rs` (small), `tools/images.rs` (bigger,
  uses jobs and dialogs).
- `transfer.rs`, `share*.rs`, `sharing.rs`, `funnel.rs`, `tls.rs`, `updater.rs`: done and
  security-sensitive. Don't change them unless a task needs it, and then say exactly what and why.

## What's left (in this order)

1. File converter (`toolkit/convert.py`): reuse `tools/images.rs`, `exif.rs`, `webp.rs`.
2. Video trimmer (`toolkit/trimmer.py`) and downloader (`toolkit/downloader.py`), via ffmpeg/yt-dlp
   found with `program()`.
3. Network devices (`netscan.py`), PC exposed (`exposure.py`), ad blocker (`adblock.py`),
   speed test (`speedtest.py`), currency (`currency.py`).
4. PC remote (`remote.py`), send to contacts (`Source/peers.py`), casting (`Source/cast.py`,
   `airplay.py`, `miracast.py`).
5. Calculator: run `calc.py` as a Python helper and pass its answers through.
6. Setup, uninstall, Explorer menu (`Source/setup`, `uninstall.ps1`, `context_menu.py`),
   shipping ffmpeg and pdfium.dll next to the exe. The release pipeline is the owner's job:
   don't touch release signing.

To find which page calls a tool needs, search `Source/web/js` for `api.` calls and the matching
`def` in `Source/swiftlane.py`.

## How to check your work

- `cd Rust && cargo test` (on Linux it needs the webkit2gtk dev packages; on Windows it just works).
- Every push to `main` that touches `Rust/` is built and tested on Windows by
  `.github/workflows/windows-rust.yml`. A tool is only "done" when that run is green.
- Write tests for the logic (maths, file formats, parsing). **Tests must only create or delete
  files inside a fresh temp folder.**
- You can't see the window from the command line, so say what you couldn't check (for example
  "the dialog wasn't tested by hand").
- One tool per commit, with a plain message saying what it does. When a tool is done, update the
  "Done" and "Next" lists in `CLAUDE.md`.

## Safety rules that matter most here

- Never put keys, passwords or tokens in the repo. The release signing key is not in the repo and
  never should be.
- Don't weaken checks in the phone server, share links, vault or updater.
- Helper programs only from next to the exe or absolute PATH entries (`program()` already does this).
- Files from the internet or other devices get the "downloaded from the internet" mark, the same
  way `share.rs` does it.
