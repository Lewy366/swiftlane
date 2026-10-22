# Swiftlane

> **About this project:** Swiftlane is a personal test project. Its code was written by AI (Anthropic's Claude) under the owner's direction, and the source is public so anyone can see exactly what the app does. It is provided as-is, without warranty.
>
> **All rights reserved.** No licence is granted: you may read the code, but not copy, modify or redistribute it without the owner's written permission. Contributions are not accepted.

Your own toolkit for Windows: AirDrop-style file transfer with your phone, plus everyday tools. Everything runs on your PC; nothing goes through a third-party service except the currency rates, the speed test and video downloads, which need the internet by nature.

- **Phone transfer** — send files between your phone and this PC over your own network. Nothing to install on the phone.
- **Share links** — send files or folders to anyone with a link, straight from this PC (through Tailscale), with a time limit, download or per-person limits and an optional password.
- **Request files** — a link people use to send files *to* you, with size and file limits and an optional password.
- **PC remote** — media keys, volume and arrow keys from your phone (off until you switch it on).
- **Security tools** — an encrypted **Vault**, **Network devices** (who's on your home network) and **Is my PC exposed?**
- **File converter**, **Image editor**, **PDF tools**, **Video trimmer**, **Disk space** map, **Video downloader**, **Currency converter**, **Calculator** (Casio ClassWiz style), **Speed test**, **Ad blocker test**.
- Right-click any file in File Explorer → *Send to phone*, *Share with a link* or *Convert*.
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

### Request files
On **Share links**, *Request files* makes a link others use to upload files to a folder you pick. You set how long it works, the most files and the biggest total size, and optionally a password. The link only accepts uploads: it never shows or offers anything that's already in the folder or what others sent. Received files are marked as coming from the internet (Windows warns before running them) and never overwrite anything.

### PC remote
Off until you switch it on in Settings. Paired phones then get a *Remote* tab: play/pause, next/previous, volume, mute and arrow keys (for slides). Sleep and shut down are a separate switch; shut down waits 60 seconds and can be cancelled.

### PDF tools
Drop one or more PDFs: pages appear as a board you can reorder by dragging, rotate, remove and select. Save as a new PDF (several PDFs are merged in order), **split** (selected pages start new parts, or every page on its own), **sign** (draw your signature or use a picture, then click where it goes) or **make smaller** (turns pages into pictures). The originals are never changed.

### Video trimmer
Pick start and end on a filmstrip. *Fast cut* (instant, no quality loss, starts at the nearest keyframe), *Exact cut*, *Fit a size* (Discord 10 MB, email 20 MB, WhatsApp 64 MB or your own) or *Sound only* (MP3).

### Disk space
Pick a drive or folder and see what takes up the room as a map of blocks you can click into, plus the biggest files. Junctions and links aren't followed, so nothing is counted twice. *Move to the Recycle Bin* is the only way to remove something (so it can be restored); it's never offered for Windows, program folders, app data or your user folders, nor on drives without a Recycle Bin (USB sticks, network drives).

### Vault
Files locked with your password. A random 256-bit key encrypts every file with AES-256-GCM in 1 MB pieces; your password (stretched with scrypt) only unlocks that key, so changing the password is instant. File names are encrypted too. Each piece is tied to its file and position, so a changed, swapped or cut-off file refuses to open instead of giving back damaged data. Opened files go to a private temporary folder that's wiped when the vault locks, which it does after a few quiet minutes and when Swiftlane closes. **There is no password recovery.** The vault is a normal folder: back it up or move it to a USB stick and open it on another PC with Swiftlane.

### Network devices
Who's on your home network: name (from your router), address, hardware address, whether it's the router, this PC, or a phone-style private address. Only your own private /24 network is checked, with one ping per address plus Windows' own address table; no ports are tried. Devices are remembered on this PC, so new ones are marked, and you can name them. Optionally, Swiftlane checks every 10 minutes and notifies you when a new device joins.

### Is my PC exposed?
A read-only check: is the firewall on, is this network Public or Private, is Remote Desktop or the old SMB1 file sharing on, which programs accept connections from the network (and what they are), whether your router opens ports to the internet by itself (UPnP) and which, and what Swiftlane itself has open. Fixes are buttons you click; changes to Windows ask for admin permission.

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
- Phone transfer can be encrypted (HTTPS): Swiftlane makes its own certificate authority, limited by name constraints to private and Tailscale addresses (it can't vouch for any website), with its key protected by Windows (DPAPI). Install it on the phone once from *Connect phone*; then *Only allow encrypted connections* refuses plain HTTP.
- Request links accept uploads only, within the limits you set; uploads are written to a temporary file and only kept when complete; received files get Windows' "from the internet" mark.
- PC remote is off by default, needs the pairing code, accepts only a fixed list of keys and is rate-limited.
- The vault never stores or sends your password; see *Vault* above for how files are encrypted.
- Network devices only pings your own home /24 network; Is my PC exposed? only reads, and talks to nothing but your router (and only at its own address).
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
