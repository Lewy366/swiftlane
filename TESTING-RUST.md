# Hands-on Test Checklist (Swiftlane Rust)

Use this checklist to verify the Rust build (`Swiftlane-Rust-Windows` artifact from the **Windows app (Rust)** GitHub Actions run) on a real Windows PC before cutting a signed release.

## 0. Bundle sanity check
- [ ] Unzip `Swiftlane-Rust-Windows` into a test folder (not `Program Files` yet).
- [ ] Confirm all files are present next to `Swiftlane.exe`: `ffmpeg.exe`, `ffprobe.exe`, `pdfium.dll`, `yt-dlp.exe`, `toolkit\calc.py`, `uninstall.ps1`, `airplay-install.ps1`, and `licenses\SOURCES.txt`.
- [ ] Launch `Swiftlane.exe`. Confirm existing settings/pairing code load cleanly (if upgrading from the Python version) or fresh defaults are created.

## 1. Window, settings & single instance
- [ ] Resize window down to 400 px (sidebar collapses behind ☰ menu; columns stack).
- [ ] Press **Ctrl+K** and jump to a tool.
- [ ] Switch **Light / Dark / Auto**, accent colour, language, and a custom background picture (with blur/darkening).
- [ ] Toggle **Start with Windows** on and off (verify HKCU Run entry).
- [ ] Launch `Swiftlane.exe` a second time — confirm it focuses the open window instead of opening a second copy.

## 2. Phone transfer & HTTPS
- [ ] Reveal QR on *Connect phone*, scan from a phone on the same Wi-Fi, verify the phone web app loads.
- [ ] Send a file **PC → phone** and **phone → PC**; confirm sorting into `Photos`/`Videos`/etc. and that sending the same filename twice does not overwrite.
- [ ] Open `/cert` on the phone, install the CA certificate, turn on **Only allow encrypted connections**, and confirm HTTPS works while plain HTTP is refused.
- [ ] Try a wrong pairing code (confirm rejection; optional: verify lockout notice).

## 3. PC remote
- [ ] Confirm *Remote* is **off** by default and `/api/remote` is refused while off.
- [ ] Switch **PC remote** on in Settings; verify the *Remote* tab appears on the paired phone.
- [ ] Test **Play/Pause**, **Next/Previous**, **Volume Up/Down/Mute**, and **Arrow keys**.
- [ ] Verify **Sleep / Shut down** stay blocked until the separate power switch is enabled (optional: test shut down + Cancel within 60 s).

## 4. Share links & Request files
- [ ] Share a single file and a folder via Tailscale Funnel; open the link from another network/phone and download both individual files and **Download all (.zip)**.
- [ ] Test an optional password and a download cap.
- [ ] Create a **Request files** link, upload a file from a phone/other browser, verify it never overwrites existing files and carries Windows' Mark of the Web (`Properties → Unblock`).
- [ ] End all links and verify Tailscale Funnel switches back off.

## 5. Send to contacts (2 PCs)
- [ ] On PC A and PC B (Rust ↔ Rust or Rust ↔ Python), copy and add each other's `SL1.` contact codes.
- [ ] Confirm the 16-character **Check** code matches on both PCs.
- [ ] Send 1 file and a multi-file batch from PC A to PC B; confirm the **Accept / Decline** popup appears on PC B and nothing is written until **Accept**.
- [ ] Confirm received files appear in PC B's Received folder with Mark of the Web, and declining/cancelling leaves no `.part` temp file behind.

## 6. Screen mirroring (Casting)
- [ ] **AirPlay install & mirroring (iPhone/iPad/Mac):**
  - [ ] Click **Download and install**; approve UAC; confirm SHA-256 verification succeeds and UxPlay installs into `Program Files\Swiftlane AirPlay` with its private-network firewall rule.
  - [ ] Switch on *Available on this network*, mirror from an iPhone/iPad/Mac, enter the 4-digit PIN, test **Alt+Enter** full screen, then test **Forget all**.
- [ ] **Miracast (Android):**
  - [ ] Check Wi-Fi support status; if needed click *Set up Wireless Display*, then *Start receiving*, and cast from an Android phone.
- [ ] **Browser casting (laptop):**
  - [ ] Switch on *Allow casting from a browser*, open the HTTPS cast link on a laptop, click **Share screen**.
  - [ ] Approve the **Allow / Deny** prompt on the PC; confirm `cast-view.html` opens; test double-click / **F11** / **Esc** full screen; switch off and verify access stops.

## 7. Everyday tools
- [ ] **File converter:** Convert image → JPG/WebP/AVIF/PDF, PDF → PNG (`pdfium.dll`), video → MP4/GIF/MP3 (`ffmpeg.exe`); verify originals are untouched.
- [ ] **Image editor:** Open JPG/PNG (plus AVIF/HEIC if handy), crop, resize, rotate/straighten, adjust brightness/contrast, strip EXIF, save copy.
- [ ] **PDF tools:** Open a multi-page PDF (confirm `pdfium.dll` page thumbnails render), reorder/rotate/delete pages, merge 2 PDFs, split, place a signature, and test **Make smaller**.
- [ ] **Video trimmer:** Open an MP4 (confirm filmstrip renders), test *Fast cut*, *Exact cut*, *Fit a size*, and *Sound only (MP3)*.
- [ ] **Video downloader:** Paste a YouTube link (confirm metadata preview via `yt-dlp.exe`), download MP4 and MP3 (merged via `ffmpeg.exe`), and test cancelling mid-download.
- [ ] **Calculator:** Test exact math (`1/3 + 1/6`, `S⇔D`), SOLVE/integral (verifies `toolkit\calc.py` + Python/SymPy), unit conversions, and **Pop out** window (*Keep on top*, shared `Ans`/memory).
- [ ] **Currency converter:** Convert between currencies; verify cached rates still work offline.
- [ ] **Speed test:** Run ping, jitter, download, and upload against Cloudflare.
- [ ] **Ad blocker test:** Run check and verify DNS servers and group results display.
- [ ] **Disk space:** Scan a normal folder, drill into blocks, verify Recycle Bin is never offered on `C:\Windows`, `Program Files`, or user profile roots.
- [ ] **Vault:** Open an existing Python-created vault (if you have one) and create a new vault; add a file, open it, change password, lock vault, and verify the temporary copy is wiped.
- [ ] **World clock:** Verify day/night shading on the map, add/remove a city, drag **Plan a time**, click *Back to now*.
- [ ] **Network devices:** Scan home `/24`, label a device, acknowledge new devices, toggle 10-minute monitor.
- [ ] **Is my PC exposed?:** Run read-only audit and confirm firewall, network profile, listening apps, and UPnP checks populate cleanly.

## 8. Explorer right-click menu
- [ ] Enable Explorer integration in Settings.
- [ ] Right-click a file in File Explorer and test **Send to phone**, **Share with a link**, **Send to contacts**, and **Convert** — once with Swiftlane already open (IPC) and once with Swiftlane closed (cold start).
- [ ] Disable Explorer integration in Settings and verify the menu entries disappear.

## 9. Uninstaller (`Swiftlane.exe --uninstall`)
- [ ] Run `Swiftlane.exe --uninstall` from a scratch install copy (keep your personal data backed up).
- [ ] Confirm **"Also delete the files Swiftlane made"** is **unchecked by default**.
- [ ] Test cancelling UAC (nothing machine-wide removed), then approve UAC on the scratch install and verify shortcuts, `HKLM\...\Uninstall\Swiftlane`, firewall rules, `Swiftlane AirPlay`, and the install folder are removed cleanly.
