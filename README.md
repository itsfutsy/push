<div align="center">

<img src="src-tauri/icons/128x128@2x.png" width="112" height="112" alt="Push app icon">

# Push

**Screenshot. Upload. Link. On your own server.**

Press <kbd>⇧</kbd><kbd>⌘</kbd><kbd>2</kbd> (<kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>2</kbd> on Windows and Linux), drag over anything on screen,<br>
and a link is on your clipboard a second later.<br>
Your files live on your own server. No accounts, no subscriptions, no one else's cloud.

[![License: MIT](https://img.shields.io/badge/license-MIT-4b5563)](LICENSE)
[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-support-FFDD00?logo=buymeacoffee&logoColor=black)](https://buymeacoffee.com/futsy)

[Get started](#get-started) · [Server guide](#server-guide) · [FAQ](#faq) · [Build from source](#build-from-source) · [Support Push](#support-push)

<br>

https://github.com/user-attachments/assets/718c1741-426a-442c-8ed5-d7b37846b530

</div>

## How it works

Push has two parts:

- **The app** on your computer. It takes the screenshot (or any file), uploads it, and copies the link.
- **The server** that you run. It stores your files and shows them to anyone who has the link.

Links look like `https://share.example.com/i/Xc3nzK9qPa.png`. They are long and random, so nobody can guess them.
Images and videos preview inline in Slack, Discord, X, Telegram and WhatsApp.

## Get started

### 1. Set up the server

You need:

- **A server with Docker.** The smallest VPS from any provider is enough. Intel and ARM both work.
- **A domain for your links**, like `share.example.com`. In your DNS settings, add an `A` record that points it at
  your server's IP address.

Then follow **A** or **B**.

#### A. On a plain server

This runs Push together with [Caddy](https://caddyserver.com), which sets up HTTPS for you. Make sure ports 80 and
443 are open, then run this on your server:

```sh
mkdir push && cd push
curl -fsSLO https://raw.githubusercontent.com/itsfutsy/push/main/docker-compose.yml
echo "PUSH_DOMAIN=share.example.com" > .env          # use your domain
echo "PUSH_TOKEN=$(openssl rand -hex 32)" >> .env    # creates your token
docker compose up -d
```

Run `cat .env` to see your token. You'll paste it into the app in step 3.

#### B. On Dokploy, Coolify or a similar tool

These tools set up HTTPS for you. Create an app from a Docker image, with these settings:

| Setting | Value |
|---|---|
| Image | `ghcr.io/itsfutsy/push:latest` |
| Port | `3000` |
| Volume | A persistent volume mounted at `/data`. This is where your files are kept. |
| `PUSH_TOKEN` | Your token: a long random password. Create one with `openssl rand -hex 32`. |
| `PUSH_PUBLIC_URL` | Your address, like `https://share.example.com` |

Then add your domain to the app in your tool, and deploy it.

#### Check that it works

Open `https://share.example.com/healthz` in your browser. It should say `ok`. If it doesn't, see
[Troubleshooting](#troubleshooting).

### 2. Install the app

Download it from the [latest release](https://github.com/itsfutsy/push/releases/latest).

- **Mac** (Apple silicon, macOS 14 or later): open the `.dmg` and drag Push into Applications.
- **Windows** (10 or 11): run the `x64-setup.exe` installer. If Windows SmartScreen warns you, click
  **More info > Run anyway**. See [Windows notes](#windows-notes).
- **Linux**: install the `.deb`, or run the `.AppImage`. See [Linux notes](#linux-notes).

### 3. Connect

Open **Push > Settings**. Enter your server's address (like `https://share.example.com`) and paste your token. When
it says **Connected**, you're done.

Press <kbd>⇧</kbd><kbd>⌘</kbd><kbd>2</kbd> (<kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>2</kbd> on Windows and Linux) and
drag over part of your screen. The first time, macOS asks for permission to record the screen: turn on Push in
**System Settings > Privacy & Security > Screen & System Audio Recording**.

## Using Push

| To | Do this |
|---|---|
| Capture part of the screen | <kbd>⇧</kbd><kbd>⌘</kbd><kbd>2</kbd> and drag. Press <kbd>Space</kbd> to capture a window instead, <kbd>Esc</kbd> to cancel. On Windows, the shortcut opens Windows' snipping bar, which can also capture a window or the whole screen. |
| Upload a file | Drop it on the Push window, click the drop area, or use **Share > Push** in Finder. On Windows, right-click it and choose **Send to > Push**. |
| Upload several files | Drop them together. All their links are copied, one per line. |
| Copy a link again | Click it in Recent, or open it in the Library and choose **Copy Link**. |
| Browse everything | **Show All** in the Push window, or **Library** in the menu bar (the tray on Windows and Linux). Press <kbd>Space</kbd> to preview and the arrow keys to move. |
| Delete | Hover a file and choose Delete, or select files in the Library and choose Delete. The link stops working right away. |
| Change the shortcut | **Settings > Capture Shortcut**. Holding both <kbd>⌘</kbd> keys works too (it needs Input Monitoring permission). |

If Push is missing from Finder's Share menu, turn it on in **System Settings > General > Login Items & Extensions**,
under **Extensions > Sharing**.

## Server guide

### Settings

| Setting | What it does |
|---|---|
| `PUSH_TOKEN` | **Required.** Your upload password, at least 32 characters. Create one with `openssl rand -hex 32`. Anyone who has it can upload and delete files, so keep it secret. |
| `PUSH_PUBLIC_URL` | **Required.** Your server's address, like `https://share.example.com`. Links start with this. With setup A, set `PUSH_DOMAIN=share.example.com` instead. |
| `PUSH_ROOT_REDIRECT` | Optional. Where to send people who open `https://share.example.com/` itself, like your homepage. Without it, they see "not found". |
| `PUSH_DATA_DIR` | Optional. Where files are saved. Default `/data`. |
| `PORT` | Optional. The port the server listens on. Default `3000`. |

With setup A, settings live in the `.env` file. Run `docker compose up -d` after you change them.

To change your token, set a new one, restart the server, and paste it into Settings in each app.

### Update

- **Setup A:** `docker compose pull && docker compose up -d`
- **Setup B:** redeploy the app in your tool so it downloads the new image.

### Back up

Your files are normal files, in the `files` folder of the `/data` volume. There is no database. With setup A, this
saves them all to `push-backup.tgz`:

```sh
docker run --rm -v push_push-data:/data -v "$PWD":/backup alpine tar czf /backup/push-backup.tgz -C /data .
```

### Use your own reverse proxy

Already run nginx, Traefik or Caddy? Run only the Push image and point your proxy at port 3000. For example:

```sh
docker run -d --name push --restart unless-stopped -p 127.0.0.1:3000:3000 \
  -e PUSH_TOKEN=your-token -e PUSH_PUBLIC_URL=https://share.example.com \
  -v push-data:/data ghcr.io/itsfutsy/push:latest
```

Then set your proxy to:

- **Allow requests of at least 10 MB.** Push sends big files in 8 MB pieces, so any file size works.
- **Wait at least 60 seconds** before timing out a request.
- **Never cache** responses, so a deleted file stops working right away.

### Use Cloudflare

Cloudflare works well in front of Push. With setup A, set the DNS record to **DNS only** (grey cloud) until Caddy
has its certificate. Then switch it to **Proxied** and set SSL/TLS to **Full (strict)**.

1. **Caching > Cache Rules:** add a rule for your domain with **Bypass cache**, so deleted files stop working right
   away.
2. Turn these **off**, because they leak your links or break them:
   - Caching > Configuration > **Always Online** (sends your links to the Internet Archive)
   - Caching > Configuration > **Crawler Hints** (sends your links to search engines)
   - Security > Bots > **Bot Fight Mode** (blocks link previews in apps like Discord)
   - Security > Bots > **Managed robots.txt** (replaces Push's own robots.txt)
   - Scrape Shield > **Hotlink Protection** (breaks embeds)
3. If a **Redirect Rule** sends your whole domain somewhere else, limit it to `/` only, or every link will redirect
   too. `PUSH_ROOT_REDIRECT` can do this for you.

Cloudflare's 100 MB request limit is no problem, because Push sends 8 MB pieces.

### Troubleshooting

| Problem | Fix |
|---|---|
| With setup A, the site doesn't load | Caddy couldn't get an HTTPS certificate. Check that your `A` record points at this server and that ports 80 and 443 are open. With Cloudflare, use **DNS only** until it works. `docker compose logs caddy` says what went wrong. |
| The server keeps restarting, and its log says `cannot write to /data/files: Permission denied` | The data folder belongs to the wrong user. This is common when you mount a folder instead of a volume. Run `sudo chown -R 65532:65532 /path/to/folder`, then restart. |
| The app says your token is wrong (`401`) | Check `PUSH_TOKEN` for a missing or extra character, then restart. The server logs every rejected request. |
| Big uploads fail on slow connections (`504` or a reset) | Your proxy times out too soon. Raise its read timeout to 600 seconds. On Traefik, that's `entryPoints.websecure.transport.respondingTimeouts.readTimeout`. |
| Uploads fail with `413` | Your proxy or Cloudflare blocks large requests. Allow at least 10 MB. |
| Links open a different website | A redirect rule on your domain also catches your links. Limit it to `/` only, or use `PUSH_ROOT_REDIRECT`. |

To test your server and token from a terminal:

```sh
curl https://share.example.com/healthz                    # ok
curl -s -o /dev/null -w "%{http_code}\n" -H "Authorization: Bearer YOUR_TOKEN" \
  https://share.example.com/api/ping                      # 204 means the token works, 401 means it doesn't
```

## FAQ

<details>
<summary><b>Do I need a domain?</b></summary>
<br>
Yes. Links need an HTTPS address. A subdomain of a domain you already own is perfect.
</details>

<details>
<summary><b>Can I use it on more than one computer?</b></summary>
<br>
Yes. Install the app on each one and give them the same server address and token. Each keeps its own upload history.
</details>

<details>
<summary><b>How much does it cost to run?</b></summary>
<br>
Only what your server costs. The server is one small program, so the cheapest VPS is plenty. Storage is just your
files.
</details>

<details>
<summary><b>How private is it?</b></summary>
<br>

- **Only you can upload.** Every upload and delete needs your token. The server checks it before reading any data,
  and gives the same answer for a wrong token as for a missing one.
- **Your token stays on your computer.** It's kept in the macOS Keychain (Credential Manager on Windows, Secret
  Service on Linux). The app never follows redirects, so it can't be tricked into sending your token anywhere else.
- **Links can't be guessed.** Each file gets a name of 10 random characters, and there are no lists or index pages.
- **Nothing gets indexed.** The server tells search engines and archives to stay away. Only link-preview bots may
  fetch files.
- **Shared files can't attack viewers.** HTML, SVG, scripts and PDFs always download. They never open as pages on
  your domain.
- **The Finder Share extension is locked down.** It has no network or keychain access. It only passes files to the
  app.

</details>

## Windows notes

- Push uses Windows' own snipping bar (the one behind <kbd>Win</kbd><kbd>Shift</kbd><kbd>S</kbd>) to capture, so it
  needs Snipping Tool, which comes with Windows. Depending on Snipping Tool's settings, Windows may also save a copy in
  Pictures > Screenshots.
- Upload from File Explorer with **Send to > Push**, or from a terminal with `push FILE`.
- The installer isn't signed yet, which is why SmartScreen warns you the first time.

## Linux notes

- Global shortcuts work on X11. On Wayland, set a system shortcut that runs `push --capture`
  (GNOME: Settings > Keyboard > Custom Shortcuts).
- Upload from a terminal with `push FILE`. For an "Upload with Push" item in Nautilus, save
  `#!/bin/sh` + `exec push "$@"` as `~/.local/share/nautilus/scripts/Upload with Push` and make it executable.
- The tray icon needs a StatusNotifier host (on GNOME, the AppIndicator extension).

## Build from source

**Server image.** Compiling needs 1 to 2 GB of RAM (add swap on small servers) and Docker's buildx plugin (included
when you install Docker from get.docker.com). Tag the image like this and setup A uses it instead of downloading one:

```sh
git clone https://github.com/itsfutsy/push.git && cd push
docker build -t ghcr.io/itsfutsy/push:latest server
```

**Server without Docker.** Run it behind any HTTPS reverse proxy pointed at port 3000, and keep it running with
systemd or similar:

```sh
cd server && cargo build --release
PUSH_TOKEN=... PUSH_PUBLIC_URL=https://share.example.com PUSH_DATA_DIR=/var/lib/push ./target/release/push-server
```

**Mac app.** Needs Xcode's command line tools and Rust. This builds `Push.app`, installs it in `/Applications` and
turns on its Share extension:

```sh
xcode-select --install                               # once, if you don't have it
curl https://sh.rustup.rs -sSf | sh                  # once, if you don't have Rust
cargo install tauri-cli --version "^2.12" --locked   # once
git clone https://github.com/itsfutsy/push.git && cd push
scripts/build-macos.sh --install
```

**Windows app.** Install [Rust](https://rustup.rs), then run `cargo tauri build --bundles nsis` in `src-tauri`.

**Linux app.** Install the packages listed in [`.github/workflows/linux.yml`](.github/workflows/linux.yml), then run
`cargo tauri build --bundles deb,appimage` in `src-tauri`.

## Development

| Folder | What it is |
|---|---|
| [`src-tauri/`](src-tauri), [`ui/`](ui) | The desktop app: Tauri 2 and Rust, with a plain HTML, CSS and JS interface. |
| [`macos/share/`](macos/share) | The Finder Share extension (Swift). |
| [`server/`](server) | The server: Rust, one program, no database. |
| [`dev/`](dev) | A browser preview of the interface with fake data, for working on the UI without the app. |

```sh
cd server && cargo run                               # server (needs PUSH_TOKEN, PUSH_PUBLIC_URL and PUSH_DATA_DIR)
cd src-tauri && cargo tauri dev                      # desktop app (PUSH_TOKEN=... skips the Keychain in debug builds)
python3 dev/serve.py                                 # UI preview: http://127.0.0.1:5174/dev/?s=list
scripts/check-linux.sh                               # compile the desktop app for Linux in Docker
```

Pushing a `v*` tag builds a release: the Mac `.dmg`, the Windows installer, the Linux `.deb` and `.AppImage`, and the
server image at `ghcr.io/itsfutsy/push`.

## Support Push

Push is free for everyone, businesses included. If it saves you time, please consider
[buying us a coffee](https://buymeacoffee.com/futsy). And if your company uses Push for its team, a sponsorship is
hugely appreciated and keeps the project going.

<a href="https://buymeacoffee.com/futsy"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-support%20Push-FFDD00?style=for-the-badge&logo=buymeacoffee&logoColor=black" alt="Buy Me a Coffee"></a>

## License

Copyright (C) 2026 Abram Industries Pty Ltd

Push is open source under the [MIT License](LICENSE).
