# Swiftlane

> **About this project:** Swiftlane is a personal test project. Its code was written by AI (Anthropic's Claude) under the owner's direction, and the source is public so anyone can see exactly what the app does. It is provided as-is, without warranty.
>
> **All rights reserved.** No licence is granted: you may read the code, but not copy, modify or redistribute it without the owner's written permission. Contributions are not accepted.

Your own toolkit for Windows: AirDrop-style file transfer with your phone, plus everyday tools. Everything runs on your PC; nothing goes through a third-party service except the currency rates, the speed test and video downloads, which need the internet by nature.

- **Phone transfer** — send files between your phone and this PC over your own network. Nothing to install on the phone.
- **Share links** — send files or folders to anyone with a link, straight from this PC (through Tailscale), with a time limit, download or per-person limits and an optional password.
- **File converter**, **Image editor**, **Video downloader**, **Currency converter**, **Calculator** (Casio ClassWiz style), **Speed test**, **Ad blocker test**.
- English, Deutsch, Polski, Italiano. Accent colours and your own background picture.

## Install

Run **Swiftlane Setup.exe** (Windows asks for admin permission). The wizard walks you through:

1. **Welcome** — if Swiftlane is already installed: *Update* (keeps your settings, pairing code and files) or *Uninstall*.
2. **Location & shortcuts** — Program Files by default; Start menu, desktop icon, start with Windows.
3. **Features** — pick the tools you want (Node.js check for the video downloader).
4. **Your files** — where received files go, and whether they're sorted into folders (Photos, Videos, Audio, Documents, Archives, Other); where video downloads are suggested.
5. **Phone & network** — a firewall rule for Swiftlane only (port 8765, private networks only), and which of your networks to mark *Private*.
6. **Personalise** — accent colour (live preview), calculator angle unit, notifications.
7. **Ready** — a summary; then it installs and offers to open Swiftlane and pair your phone.

**Rollback:** every install step records how to undo itself. If anything fails (a locked file, the firewall service, disk full…), Setup undoes every step and your PC is exactly as before, including the previous Swiftlane version if you were updating.

## Uninstall

**Settings → Apps → Swiftlane → Uninstall.** It removes everything Swiftlane set up: the program, firewall rule, shortcuts, the Settings → Apps entry, start-with-Windows, your Swiftlane settings and logs, and leftovers of older versions (PocketDrop, the downloader's cache).

The box **"Also delete the files Swiftlane made"** is **off by default**. When ticked, it deletes only files Swiftlane itself created (received, converted, edited, downloaded); files you saved there yourself are never touched, and only folders that end up empty are removed.

## The tools

### Phone transfer & Connect phone
- **Connect phone:** click the blurred QR code to reveal it and scan it with your phone's camera. That opens the Swiftlane web app (works on iPhone and Android); add it to your home screen to use it like an app.
- **Send to phone:** drag files onto the drop zone; they appear under *From PC* on the phone.
- **Received:** files from your phone arrive live, sorted into folders if you chose that.
- **iPhone share sheet:** make a *Send to PC* Shortcut (steps below) to send from any app via Share.

### Share links
Drop files or folders on **Share links** and the link is copied straight away. Nothing is uploaded anywhere: the files stay where they are and are sent from this PC when someone downloads, so **this PC has to be on and Swiftlane open** for links to work. Speed is your internet upload speed.

- **For each share:** how long it's available (1 hour to 30 days, extend or end any time), and who can download: *anyone with the link*, with an optional limit on how many downloads (change it any time with *Download limit…*), or *personal links* (one link per person, each person downloads once; add people later, block one person without affecting the others). Optional password.
- **What counts as a download** (anyone-with-the-link): every time a file is downloaded, and *Download all* (.zip) counts once. Finishing a broken download doesn't count again; chat apps opening the link for a preview don't count. *What's included* shows how many times each file was downloaded.
- **Personal links:** a person's visit can take every file; their link is used once they've downloaded.
- **Folders** are listed once when you share them: files added later aren't included; hidden files, system files and shortcuts/links are left out. "What's included" shows exactly what's shared. Swiftlane asks first before sharing a whole personal folder or drive, or files that look private (keys, passwords, wallets), and never shares Windows, program or app-data folders.
- **Recipients** get a plain download page in any browser: each file, or everything as one .zip. Downloads can resume.

**How it's reached:** through **Tailscale Funnel**. The link looks like `https://<pc-name>.<tailnet>.ts.net/s/…`; it shows your PC's Tailscale name but not your IP or address, and no port is opened on your router. The encryption ends on your PC, so Tailscale's relay can't read the files. Swiftlane switches Funnel **on only while a link can be used** (plus 15 minutes after a used-up link's last download, so it can resume), and **off** when the last link ends, when Swiftlane closes, and on uninstall. It never touches Funnel/Serve settings that something else made.

**One-time setup** (in your Tailscale account, the first time you share): Tailscale asks you to turn on HTTPS certificates and allow Funnel for this PC. Swiftlane shows an *Open Tailscale* button for it. Also turn on two-factor sign-in for your Tailscale account: whoever controls the account controls Funnel.

### File converter
Images (jpg, png, webp, gif, bmp, tiff, ico, avif, heic → jpg, png, webp, gif, bmp, tiff, ico, avif, pdf), video (mp4, mov, mkv, avi, webm… → mp4, mov, mkv, avi, webm, gif, or audio only), audio (mp3, wav, m4a, aac, flac, ogg, opus…), images ↔ PDF. Quality *High / Balanced / Smaller file*. Swiftlane asks where to save: a "Save as" box for one file, a folder for several.

### Image editor
Live preview you work on with the mouse: draw and drag a **crop** box (free or locked ratio), drag the corner to **resize**, **rotate/flip**, **straighten**, adjust **brightness, contrast, saturation, sharpness**, choose format and quality, optionally remove location & camera info. Shows the new size and estimated file size as you edit; *Hold: original* and *Exact preview* compare before/after. Saving asks where.

### Video downloader
YouTube and the many other sites [yt-dlp](https://github.com/yt-dlp/yt-dlp) supports: best quality, 1080p, 720p, 480p, MP3 or M4A; whole playlists. Asks where to save. YouTube needs Node.js for some formats. Only download what you have the right to save. Rebuilding the setup always takes the newest yt-dlp.

### Currency converter
European Central Bank rates for 30 currencies (free Frankfurter service, no account), refreshed every working day, kept for offline use.

### Calculator
Modelled on the Casio fx-87DE X ClassWiz: exact results (fractions, √, π) with **S⇔D**, Deg/Rad/Gra and Norm/Fix/Sci (click the labels on the display), ∫, d/dx, Σ, Π, SOLVE, variables, Ans/PreAns, M+/M−, Pol/Rec, GCD/LCM, ÷R, FACT, plus modes for Complex, Base-N, Statistics (7 regressions), Distribution, Equation (systems, polynomials, inequalities), Matrix, Vector, Table, Ratio, 35 constants and 21 unit conversions.

### Speed test
Ping, jitter, download and upload against Cloudflare's nearest server, with a live gauge. Cloudflare limits how often one connection can test; if you run many tests in a row it asks you to wait a few minutes, and Swiftlane says so instead of showing a wrong result.

### Ad blocker test
Checks ~45 well-known ad, tracker and analytics domains in six groups and shows which are blocked, plus your DNS servers. It tests blocking for the whole PC or network (AdGuard DNS, NextDNS, Pi-hole, router, hosts file). Browser extensions like uBlock Origin only work inside the browser, so no app can detect them.

### Settings
Appearance (accent colour, background picture with blur and darkening, language), features on/off (switching off phone transfer stops Swiftlane listening on the network), where received files go and sorting, notifications, new pairing code, default download folder, calculator angle unit, start with Windows.

## iPhone "Send to PC" Shortcut

1. Open **Shortcuts** → **+** → name it **Send to PC**; in its details turn on **Show in Share Sheet**.
2. Add **Repeat with Each** → **Shortcut Input**.
3. Inside: **URL Encode** → **Repeat Item**, then tap Repeat Item and pick **Name**.
4. Below it: **Get Contents of URL** — URL: the Shortcut address from *Connect phone* (copy button), then the **URL Encoded Text** variable after `name=`. **Show More** → Method **POST**, header `X-Token` = your pairing code, Request Body **File** → **Repeat Item**.

The first time, iOS asks to let Shortcuts find devices on your local network: allow it.

## Security

- The firewall allows port 8765 for the installed Swiftlane.exe only, on private networks only. It lives in Program Files, which only admins can change, and claims the port exclusively.
- Every phone request needs the pairing code; an address is locked out for 15 minutes after 10 wrong tries. Only private-network and Tailscale addresses are accepted.
- Download links for PC → phone files are signed and expire after 10 minutes.
- Received file names are cleaned; nothing is overwritten; partial uploads are deleted.
- Transfers are plain HTTP: fine on your home Wi-Fi (WPA2/WPA3); on public Wi-Fi use the Tailscale address or switch phone transfer off.
- Share links: a separate small server that listens on this PC only (127.0.0.1:8766), reachable from outside only through Tailscale Funnel and only while something is shared. Download only; links are random 128-bit IDs and never contain file paths; pages have no scripts; every file is sent as a download. Passwords are stored as scrypt hashes; 5 wrong tries from one address (40 on one link) wait 15 minutes. At most 48 connections at once. Anyone who downloads a file can pass it on: limits control the link, not the file.

## For developers: the Source folder

Double-click **Build Setup.cmd** to rebuild `Swiftlane Setup.exe` after changing anything in `Source` (a couple of minutes; it installs the Python libraries it needs and the newest yt-dlp).

| Path | What it is |
|---|---|
| `swiftlane.py` | The app: window, features, background jobs, uninstaller (`--uninstall`). `--page <name>` opens a page; `--selftest <file>` checks every tool; `--headless` runs only the phone server. |
| `server.py` | The local server the phone talks to, settings, sorting. |
| `share.py` | Share links: what may be shared, the list of shares, the download server, Tailscale Funnel on/off. |
| `toolkit\` | The tools: `convert`, `images`, `downloader`, `currency`, `calc`, `speedtest`, `adblock`. |
| `web\` | The window (`desktop.html`, `css\`, `js\` one file per page, `i18n*.js` translations), the phone web app (`phone.html`), the setup wizard (`setup.html`, `setup\`), the uninstaller (`uninstall.html`), icons. |
| `setup\` | Setup wizard (`setup.py`), install engine with rollback (`installer.py`), packing (`make_payload.py`). |
| `uninstall.ps1` | The admin part of uninstalling (program folder, firewall rule, Settings → Apps entry). |
| `build.cmd`, `make_icons.py`, `rename-project.ps1` | Build, icon drawing, one-time project folder rename. |
