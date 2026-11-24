# Changelog

What's new in each version of Swiftlane.

## 2.2.0 (2026-11-24)

- Cleaner native look: refreshed icons throughout the app, crisp solid buttons, calm neutral file badges, and understated selection states without glowing borders.
- New offline fonts (Geist and Geist Mono) for sharper text and aligned numbers across every tool, including English, German, Polish and Italian.
- Updated Setup wizard and Uninstaller with the Interlocked SL logo, Topographic backdrop, live Dark / Light / Auto appearance picker, and all 23 tools.
- Fixed the Home screen shifting upwards when opening the slide-out menu, plus Light-mode divider and spacing polish in Setup and Vault.

## 2.1.0 (2026-11-24)

- New: Shrink to size — right-click a video or picture (or drop it in) and pick Discord 10 MB, WhatsApp 16 MB, Email 25 MB or your own limit; the smaller file lands on your clipboard, ready to paste with Ctrl+V. It warns before a long video would look bad and offers to trim it first.
- New: Phone camera & document scan — take photos with your phone's real camera from the phone page and they appear on the PC; drag four corners to flatten a document, choose colour, grey or black & white, and save as JPG, a new PDF or added to an existing PDF (location data removed).

## 2.0.1 (2026-11-23)

- New: Full-bleed Home / Greeting screen with the centered Interlocked SL logo, slide-out menu, Recent tools, Recent files (excluding Vault), and a live Updates & Patch Notes card.
- Upgraded Disk space into Storage & Cleanup with 4 tabs: Drive map, Quick clean, Downloads, and Steam games, plus a live before/after free-space bar.
- Much faster Drive map: always-on multi-threaded FindFirstFileExW scanning (~5-10x faster, no admin) plus an optional Fast NTFS MFT scan (Admin) for ~2-5 second full-drive scans.
- New: Cleanup advisor in Drive map automatically sorts findings into Safe to remove, Consider removing, and Should stay with plain-English reasons.
- Built-in Quick clean clears safe temporary files, GPU shader caches, crash dumps, and orphaned Steam game folders; Downloads tidy offers one-click archive/installer selection to Recycle Bin; Games lists Steam libraries across all drives.

## 2.0.0 (2026-11-23)

- Rebuilt Swiftlane in Rust (Tauri) for faster startup, lower memory usage, and multi-core performance — keeping all your existing settings, pairing codes, contacts, and encrypted vaults.
- New Swiftlane 2.0 look: Interlocked SL emblem & wordmark, crisp solid slate dark & light themes, Topographic built-in backdrop (custom pictures, GIFs, and videos still supported in Settings), and category-tinted icons on All tools.
- New: Duplicate finder — fast multi-core SHA-256 duplicate scan with smart keep presets; removed copies only ever go to the Recycle Bin.
- New: Find files — instant search across hundreds of thousands of files (no-admin folder scan plus an optional read-only NTFS MFT admin scan).
- New: Startup check — see everything that starts with Windows (Registry Run, Startup folders, Scheduled Tasks) with publisher info and reversible Task Manager-compatible toggles.
- New: QR Studio — create offline Wi-Fi, link, contact, email, and text QR codes (PNG/SVG/clipboard) and decode QR codes locally from dropped images or pasted screenshots.

## 1.6.1 (2026-11-22)

- AirPlay is now Screen mirroring, with two more ways in besides iPhone, iPad and Mac.
- Android phones: mirror through Windows' own Wireless Display receiver (Miracast). Swiftlane sets it up from Windows Update and opens it; pick the PC under Smart View or Screen cast on the phone.
- Laptops and computers: open a link in Chrome, Edge, Firefox or Safari and share the screen, with nothing to install. Swiftlane asks you Allow or Deny first, and the picture opens in its own window (double-click or F11 for full screen).

## 1.6.0 (2026-11-21)

- New: AirPlay. Mirror your iPhone's screen, or play its videos and music, on this PC: pick the PC under Screen Mirroring on the iPhone and the picture opens in its own window (Alt+Enter for full screen).
- It's off until you switch it on, and a new phone types a 4-digit PIN once. The receiver (the open-source UxPlay) is a one-time 77 MB download, and Windows asks once for permission to install it.

## 1.5.1 (2026-11-11)

- On iPhone (Swiftlane added to the Home Screen), Save now opens the share sheet (Save to Photos / Save to Files) so you stay on the page, instead of a download screen with no way back

## 1.5.0 (2026-11-10)

- New: pop the calculator out into its own small window that stays on top of other windows (Pop out button on the Calculator page)
- The pin button in that window switches "stay on top" on or off, and Swiftlane remembers your choice
- Both windows share one calculator, so Ans, variables and matrices are the same in each
- The pop-out window can only use the calculator and the clipboard, nothing else in Swiftlane
- Closing the main window closes the pop-out too, so Swiftlane really quits (the vault locks and share links stop)
- Swiftlane's window can now be made much smaller; in a small window the sidebar opens from a menu button

## 1.4.1 (2026-11-01)

- Fixed: a very hard sum in the calculator (like 9^9^9) could freeze the whole app; now it stops after 10 seconds with "Time limit" and everything else keeps working
- The calculator keeps Ans, variables and matrices when a sum is stopped
- Password boxes now have their own show/hide eye that stays visible, also after you click elsewhere

## 1.4.0 (2026-11-01)

- Updates: Swiftlane checks every time it starts and every 30 minutes while it's open, and shows a big popup with what's new when an update is ready
- "Later" hides the popup until Swiftlane next starts; nothing installs until you click Update now
- Share links: chat apps now show a link card with the file name, number of files and size, plus a small picture if the first file is one
- New option "Show a picture in link previews" (on by default); chat apps may keep a copy of that picture, so you can switch it off per share
- The download page shows a big preview for a single picture, small previews in the list, plain file types and a big Download button
- Programs (.exe, .msi, .bat…) get a clear warning: only run them if you trust the sender
- Previews are fresh small copies without location or camera data, never the original file, and never count as a download
- Password-protected links show no file names or pictures until the password is entered

## 1.3.1 (2026-11-01)

- Share links: Swiftlane now checks each link from the internet side and only says "Your link is ready" once it really works
- The first time Tailscale sets up your secure address (a minute or two), the Share links page shows "Getting your link ready" instead of handing out a link that doesn't open yet
- If a link can't be reached from the internet, Swiftlane says so clearly with a "Check again" button

## 1.3.0 (2026-10-24)

- New: World clock. A live world map with the night side shaded from the sun's real position, and your cities as pins
- Analog clocks for each city (light by day, dark at night) with the time difference and whether it's today or tomorrow there
- Plan a time: drag along the hours to see any moment in every city at once, then jump back to now
- Works offline, with daylight saving always right

## 1.2.0 (2026-10-24)

- New: Send to contacts. If the other person has Swiftlane too, send files straight from your PC to theirs: no link, no account, no ports to open
- Add each other once with your contact codes; a short Check code lets you confirm it's really them
- The receiver always sees who's sending what and decides: nothing arrives until they click Accept
- End-to-end encrypted; only your contacts can reach you, and only while you have at least one contact
- For people without Swiftlane, "Make a link instead" hands the files to Share links as before

## 1.1.5 (2026-10-24)

- New PC remote page: switch it on, see how to use it on your phone, try the keys on the PC, and watch presses from your phone arrive
- The phone now shows or hides the Remote tab within a few seconds when you switch it on or off, no reload needed
- Fixed: the sidebar squashed some tools on top of each other when the list was long; it now scrolls and fades at the edge
- Fixed: messages like "Swiftlane is up to date" were cut off at the bottom of the window

## 1.1.4 (2026-10-23)

- Fixed: in Setup, the Features step couldn't scroll, so you couldn't reach the Next button and finish installing

## 1.1.3 (2026-10-22)

- Fixed: the sidebar couldn't scroll when the tool list was taller than the window

## 1.1.2 (2026-10-22)

- Fixed: updating from inside Swiftlane failed because the old Swiftlane folder counted as "in use"
- Fixed: the update window was invisible when started from Swiftlane, so errors never showed
- Setup now closes leftover update windows from earlier failed tries

## 1.1.1 (2026-10-22)

- Fixed: updating from inside Swiftlane stopped right after the permission prompt and nothing was installed

## 1.1.0 (2026-10-22)

- Folders in the sidebar: arrange tools your way, plus an All tools page and Ctrl+K search
- Encrypted phone transfer (HTTPS) with Swiftlane's own certificate, and an option to refuse unencrypted connections
- Request files: a link others use to send files to you, with limits and an optional password
- PC remote: media keys, volume and arrow keys from your phone (off until you switch it on)
- Right-click any file in File Explorer: Send to phone, Share with a link, Convert
- PDF tools: merge, reorder, rotate, split, sign and make smaller
- Video trimmer: fast or exact cuts, fit a size for Discord, email or WhatsApp, or keep only the sound
- Disk space: see what fills up your drives and move things to the Recycle Bin
- Vault: files locked with your password (AES-256), locks itself when not in use
- Network devices: who is on your home network, with an optional alert when a new device joins
- Is my PC exposed?: firewall, open ports, Remote Desktop and router port forwards at a glance
- Redesigned phone page

## 1.0.0 (2026-10-22)

- Phone transfer: send files between your phone and this PC over your home network, nothing to install on the phone
- Share links: send files or folders to anyone with a link, with time limits, download limits, personal links and passwords
- File converter, image editor, video downloader, currency converter, ClassWiz-style calculator, speed test and ad blocker test
- Liquid glass design with light and dark mode, and your own picture, GIF or video as background
- English, German, Polish and Italian
- Updates: Swiftlane shows what's new and updates itself with one click; only updates signed by the owner are accepted
