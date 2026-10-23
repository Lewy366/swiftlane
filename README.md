# Swiftlane

> **About this project:** Swiftlane is a personal test project of mine. I directed the work, but the code itself was written by an AI (Anthropic's Claude). The source is public so anyone can check exactly what the app does. It comes as-is, with no warranty.
>
> **Ideas and feedback are welcome.** If you have a suggestion for a new feature, found something that doesn't work, or just want to comment on the project, open an issue on this repo's Issues tab. I read them all and I'm happy to hear new ideas.
>
> **All rights reserved.** There's no licence, so you're free to read the code but not to copy, modify or redistribute it without my written permission. Because of that I can't accept code contributions or pull requests, but suggestions and comments are always welcome.

Swiftlane is a toolkit for Windows. The main idea is AirDrop-style file transfer between your phone and your PC, and around that sits a bunch of everyday tools. Everything runs on your own PC. The only things that touch an outside service are the currency rates, the speed test and video downloads, and those need the internet anyway.

- **Phone transfer:** send files between your phone and this PC over your own network. You don't need to install anything on the phone.
- **Share links:** send files or folders to anyone with a link, straight from this PC (via [Tailscale](https://tailscale.com), a free-to-use app that connects your devices securely, more on that under *Share links*). You can set a time limit, download or per-person limits and an optional password.
- **Request files:** a link that lets people send files *to* you, with size and file limits and an optional password.
- **PC remote:** media keys, volume and arrow keys from your phone. It's off until you switch it on.
- **Security tools:** an encrypted **Vault**, **Network devices** (see who's on your home network) and **Is my PC exposed?**
- **File converter**, **Image editor**, **PDF tools**, **Video trimmer**, **Disk space** map, **Video downloader**, **Currency converter**, **Calculator** (Casio ClassWiz style), **Speed test** and **Ad blocker test**.
- Right-click any file in File Explorer and pick *Send to phone*, *Share with a link* or *Convert*.
- Available in English, Deutsch, Polski and Italiano. You can change the accent colour and use your own background picture.

## Install

Run **Swiftlane Setup.exe** (Windows will ask for admin permission). The setup wizard goes through these steps:

1. **Welcome:** if Swiftlane is already installed, you can choose *Update* (keeps your settings, pairing code and files) or *Uninstall*.
2. **Location & shortcuts:** Program Files by default, plus Start menu, desktop icon and start with Windows.
3. **Features:** pick the tools you want. It also checks for Node.js, which the video downloader needs.
4. **Your files:** where received files go, whether they get sorted into folders (Photos, Videos, Audio, Documents, Archives, Other), and where video downloads are suggested.
5. **Phone & network:** adds a firewall rule for Swiftlane only (port 8765, private networks only) and lets you choose which of your networks to mark as *Private*.
6. **Personalise:** accent colour with a live preview, calculator angle unit and notifications.
7. **Ready:** a summary of your choices, then it installs and offers to open Swiftlane and pair your phone.

**Rollback:** every install step records how to undo itself. If something fails (a locked file, the firewall service, a full disk and so on), Setup reverses every step and your PC ends up exactly as it was. If you were updating, that includes the previous Swiftlane version.

## Uninstall

Go to **Settings → Apps → Swiftlane → Uninstall.** It removes everything Swiftlane set up: the program, the firewall rule, shortcuts, the Settings → Apps entry, start with Windows, your Swiftlane settings and logs, and leftovers from older versions (PocketDrop and the downloader's cache).

There's a box called **"Also delete the files Swiftlane made"** and it's **off by default**. If you tick it, only files Swiftlane created itself get deleted (received, converted, edited and downloaded files). Anything you saved there yourself is left alone, and only folders that end up empty are removed.

## The tools

### Phone transfer & Connect phone
- **Connect phone:** click the blurred QR code to reveal it, then scan it with your phone's camera. That opens the Swiftlane web app, which works on iPhone and Android. Add it to your home screen if you want it to feel like a normal app.
- **Send to phone:** drag files onto the drop zone and they show up under *From PC* on your phone.
- **Received:** files from your phone arrive live, and get sorted into folders if you chose that.
- **iPhone share sheet:** make a *Send to PC* Shortcut (steps below) so you can send from any app using Share.

### Share links
Drop files or folders onto **Share links** and the link is copied right away. Nothing gets uploaded anywhere. The files stay where they are and are sent from this PC when someone downloads them, so **this PC has to be on and Swiftlane has to be open** for the links to work. The speed depends on your internet upload speed.

- **Settings for each share:** how long it's available (1 hour to 30 days, and you can extend or end it whenever you like), and who can download. That can be *anyone with the link*, optionally with a cap on the number of downloads (change it any time with *Download limit…*), or *personal links*, which give one link per person that each person can use once. You can add people later and block one person without affecting the others. A password is optional.
- **What counts as a download** (for anyone-with-the-link): each time a file is downloaded, and *Download all* (.zip) counts once. Finishing a broken download doesn't count a second time, and chat apps opening the link to make a preview don't count either. *What's included* shows how many times each file was downloaded.
- **Personal links:** one visit can take every file, and the link is used up once that person has downloaded.
- **Folders** are listed once, when you share them. Files added later aren't included, and hidden files, system files and shortcuts/links are left out. "What's included" shows exactly what's shared. Swiftlane asks before sharing a whole personal folder or drive, or files that look private (keys, passwords, wallets), and it never shares Windows, program or app-data folders.
- **Recipients** get a plain download page that works in any browser. They can grab each file or everything as one .zip, and downloads can resume.

**What is Tailscale?** [Tailscale](https://tailscale.com) is an app that links your devices into your own private, encrypted network, and it has a free plan for personal use. Its **Funnel** feature can also make one service on your PC reachable from the wider internet, even for people who don't use Tailscale. That's what Swiftlane uses for share links, so you only need Tailscale on the PC that does the sharing. The people downloading don't need it, and neither does your phone for normal transfers.

- **Download it:** [tailscale.com/download](https://tailscale.com/download) (Windows, plus macOS, Linux, iPhone and Android)
- **Install guide:** [tailscale.com/docs/install](https://tailscale.com/docs/install)
- **Quickstart for first-time setup:** [tailscale.com/docs/how-to/quickstart](https://tailscale.com/docs/how-to/quickstart)
- **How Funnel works:** [tailscale.com/kb/1223/funnel](https://tailscale.com/kb/1223/funnel)

**How people reach it:** through **Tailscale Funnel**. The link looks like `https://<pc-name>.<tailnet>.ts.net/s/…`. It shows your PC's Tailscale name but not your IP or address, and no port is opened on your router. The encryption ends on your PC, so Tailscale's relay can't read the files. Swiftlane only switches Funnel **on while a link can actually be used** (plus 15 minutes after a used-up link's last download, so it can resume). It switches it **off** when the last link ends, when Swiftlane closes and when you uninstall. It never touches Funnel or Serve settings that something else created.

**One-time setup** (in your Tailscale account, the first time you share): Tailscale will ask you to turn on HTTPS certificates and allow Funnel for this PC. Swiftlane shows an *Open Tailscale* button for this. You should also turn on two-factor sign-in for your Tailscale account, because whoever controls the account controls Funnel.

### File converter
- Images: jpg, png, webp, gif, bmp, tiff, ico, avif and heic, converted to jpg, png, webp, gif, bmp, tiff, ico, avif or pdf.
- Video: mp4, mov, mkv, avi, webm and more, converted to mp4, mov, mkv, avi, webm, gif, or audio only.
- Audio: mp3, wav, m4a, aac, flac, ogg, opus and more.
- Images to PDF and back.

Quality is *High*, *Balanced* or *Smaller file*. Swiftlane asks where to save, with a "Save as" box for one file or a folder for several.

### Image editor
A live preview you work on with the mouse. Draw and drag a **crop** box (free or locked ratio), drag the corner to **resize**, **rotate/flip**, **straighten**, and adjust **brightness, contrast, saturation and sharpness**. You can pick the format and quality, and optionally strip location and camera info. It shows the new size and an estimated file size as you edit, and *Hold: original* and *Exact preview* let you compare before and after. Saving asks where to put the file.

### Video downloader
Works with YouTube and the many other sites [yt-dlp](https://github.com/yt-dlp/yt-dlp) supports. You can pick best quality, 1080p, 720p, 480p, MP3 or M4A, and download whole playlists. It asks where to save. YouTube needs Node.js for some formats. Only download things you have the right to save. Rebuilding the setup always pulls the newest yt-dlp.

### Currency converter
European Central Bank rates for 30 currencies, from the free Frankfurter service (no account needed). Rates refresh every working day and are kept for offline use.

### Calculator
Modelled on the Casio fx-87DE X ClassWiz. It gives exact results (fractions, √, π) with **S⇔D**, Deg/Rad/Gra and Norm/Fix/Sci (click the labels on the display), and has ∫, d/dx, Σ, Π, SOLVE, variables, Ans/PreAns, M+/M−, Pol/Rec, GCD/LCM, ÷R and FACT. There are also modes for Complex, Base-N, Statistics (7 regressions), Distribution, Equation (systems, polynomials, inequalities), Matrix, Vector, Table and Ratio, plus 35 constants and 21 unit conversions.

### Speed test
Ping, jitter, download and upload against Cloudflare's nearest server, with a live gauge. Cloudflare limits how often one connection can run a test. If you run lots in a row it asks you to wait a few minutes, and Swiftlane tells you that instead of showing a wrong result.

### Ad blocker test
Checks about 45 well-known ad, tracker and analytics domains in six groups and shows which ones are blocked, along with your DNS servers. It tests blocking for the whole PC or network (AdGuard DNS, NextDNS, Pi-hole, your router, a hosts file). Browser extensions like uBlock Origin only work inside the browser, so no app can detect them.

### Request files
On **Share links**, *Request files* makes a link that other people can use to upload files into a folder you pick. You set how long it works, the maximum number of files and the biggest total size, and you can add a password. The link only accepts uploads. It never shows or offers anything already in the folder or anything others have sent. Received files are marked as coming from the internet (so Windows warns before running them) and they never overwrite anything.

### PC remote
Off until you switch it on in Settings. After that, paired phones get a *Remote* tab with play/pause, next/previous, volume, mute and arrow keys (handy for slides). Sleep and shut down sit behind a separate switch, and shut down waits 60 seconds and can be cancelled.

### PDF tools
Drop in one or more PDFs and the pages show up as a board. You can reorder them by dragging, rotate, remove and select pages. From there you can save as a new PDF (several PDFs get merged in order), **split** (selected pages start new parts, or every page on its own), **sign** (draw your signature or use a picture, then click where it goes) or **make smaller** (this turns pages into pictures). Your original files are never changed.

### Video trimmer
Pick the start and end on a filmstrip. Your options are *Fast cut* (instant, no quality loss, starts at the nearest keyframe), *Exact cut*, *Fit a size* (Discord 10 MB, email 20 MB, WhatsApp 64 MB or your own) and *Sound only* (MP3).

### Disk space
Pick a drive or folder and see what's taking up the room as a map of blocks you can click into, plus a list of the biggest files. Junctions and links aren't followed, so nothing gets counted twice. *Move to the Recycle Bin* is the only way to remove something, so it can always be restored. It's never offered for Windows, program folders, app data or your user folders, and not on drives without a Recycle Bin (USB sticks, network drives).

### Vault
Files locked with your password. A random 256-bit key encrypts every file with AES-256-GCM in 1 MB pieces. Your password (stretched with scrypt) only unlocks that key, so changing the password is instant. File names are encrypted too. Each piece is tied to its file and its position, so a changed, swapped or cut-off file refuses to open instead of handing back damaged data. Opened files go to a private temporary folder that's wiped when the vault locks, which happens after a few quiet minutes and whenever Swiftlane closes. **There is no password recovery.** The vault is just a normal folder, so you can back it up or put it on a USB stick and open it on another PC with Swiftlane.

### Network devices
Shows who's on your home network: the name (from your router), the address, the hardware address, and whether it's the router, this PC or a phone using a private address. Only your own private /24 network is checked, with one ping per address plus Windows' own address table, and no ports are tried. Devices are remembered on this PC, so new ones get marked and you can give them names. If you want, Swiftlane can check every 10 minutes and notify you when a new device joins.

### Is my PC exposed?
A read-only check. It looks at whether the firewall is on, whether this network is Public or Private, whether Remote Desktop or the old SMB1 file sharing is on, which programs accept connections from the network (and what they are), whether your router opens ports to the internet on its own (UPnP) and which ones, and what Swiftlane itself has open. Fixes are buttons you click, and any change to Windows asks for admin permission.

### Settings
Appearance (accent colour, background picture with blur and darkening, language), turning features on and off (switching off phone transfer stops Swiftlane listening on the network), where received files go and how they're sorted, notifications, a new pairing code, the default download folder, the calculator angle unit and start with Windows.

## iPhone "Send to PC" Shortcut

1. Open **Shortcuts**, tap **+** and name it **Send to PC**. In its details, turn on **Show in Share Sheet**.
2. Add **Repeat with Each** and set it to **Shortcut Input**.
3. Inside it, add **URL Encode** with **Repeat Item**, then tap Repeat Item and pick **Name**.
4. Below that, add **Get Contents of URL**. For the URL, use the Shortcut address from *Connect phone* (there's a copy button), then add the **URL Encoded Text** variable after `name=`. Tap **Show More**, set the Method to **POST**, add a header `X-Token` with your pairing code, and set Request Body to **File** with **Repeat Item**.

The first time you run it, iOS asks whether to let Shortcuts find devices on your local network. Say yes.

## Security

- The firewall allows port 8765 for the installed Swiftlane.exe only, and only on private networks. The exe sits in Program Files, which only admins can change, and it claims the port exclusively.
- Every phone request needs the pairing code. An address is locked out for 15 minutes after 10 wrong tries, and only private-network and Tailscale addresses are accepted.
- Download links for PC to phone files are signed and expire after 10 minutes.
- Received file names are cleaned up, nothing gets overwritten and partial uploads are deleted.
- Phone transfer can be encrypted (HTTPS). Swiftlane makes its own certificate authority, limited by name constraints to private and Tailscale addresses (so it can't vouch for any website), and its key is protected by Windows (DPAPI). Install it on your phone once from *Connect phone*, and then *Only allow encrypted connections* refuses plain HTTP.
- Request links only accept uploads, within the limits you set. Uploads are written to a temporary file and only kept once complete, and received files get Windows' "from the internet" mark.
- PC remote is off by default, needs the pairing code, only accepts a fixed list of keys and is rate-limited.
- The vault never stores or sends your password. See *Vault* above for how files are encrypted.
- Network devices only pings your own home /24 network. Is my PC exposed? only reads, and talks to nothing except your router (and only at its own address).
- Share links run on a separate small server that listens on this PC only (127.0.0.1:8766). It's reachable from outside only through Tailscale Funnel, and only while something is shared. It's download only, links are random 128-bit IDs and never contain file paths, pages have no scripts and every file is sent as a download. Passwords are stored as scrypt hashes, and 5 wrong tries from one address (40 on one link) means a 15-minute wait. There's a limit of 48 connections at once. Keep in mind that anyone who downloads a file can pass it on: the limits control the link, not the file.

## For developers: the Source folder

Double-click **Build Setup.cmd** to rebuild `Swiftlane Setup.exe` after you change anything in `Source`. It takes a couple of minutes and installs the Python libraries it needs plus the newest yt-dlp.

| Path | What it is |
|---|---|
| `swiftlane.py` | The app itself: window, features, background jobs and the uninstaller (`--uninstall`). `--page <name>` opens a page, `--selftest <file>` checks every tool, and `--headless` runs only the phone server. |
| `server.py` | The local server the phone talks to, plus settings and sorting. |
| `share.py` | Share links: what may be shared, the list of shares, the download server and Tailscale Funnel on/off. |
| `toolkit\` | The tools: `convert`, `images`, `downloader`, `currency`, `calc`, `speedtest`, `adblock`. |
| `web\` | The window (`desktop.html`, `css\`, `js\` with one file per page, `i18n*.js` translations), the phone web app (`phone.html`), the setup wizard (`setup.html`, `setup\`), the uninstaller (`uninstall.html`) and icons. |
| `setup\` | The setup wizard (`setup.py`), the install engine with rollback (`installer.py`) and packing (`make_payload.py`). |
| `uninstall.ps1` | The admin part of uninstalling (program folder, firewall rule, Settings → Apps entry). |
| `build.cmd`, `make_icons.py`, `rename-project.ps1` | Build script, icon drawing and a one-time project folder rename. |
