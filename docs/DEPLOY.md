# Self-hosting the Push server

The server is one small Rust program in a Docker image. It keeps files on disk, has no database, and needs no
maintenance. Pick the option that matches what you already have.

| You have | Use | Time |
|---|---|---|
| A fresh VPS (any Linux box with Docker) | [Option 1: Docker Compose](#option-1-docker-compose-recommended) | ~5 min |
| Coolify, Dokploy, CapRover, Traefik, nginx... | [Option 2: your own proxy or platform](#option-2-your-own-reverse-proxy-or-platform) | ~10 min |
| No Docker | [Option 3: plain binary](#option-3-without-docker) | ~10 min |

**Requirements:** a domain or subdomain you control (for example `share.example.com`), and a server with at least
1 GB of RAM. Building the image compiles Rust, which needs 1 to 2 GB, so add swap on very small machines.

## Option 1: Docker Compose (recommended)

This runs the server plus [Caddy](https://caddyserver.com), which gets and renews an HTTPS certificate for you.

1. **Point your domain at the server.** Create a DNS `A` record for `share.example.com` with your server's IP
   address. Using Cloudflare? Set it to **DNS only** (grey cloud) for now; see [Cloudflare](#using-cloudflare).
2. **Open ports 80 and 443** in your server's firewall.
3. **On the server**, clone the repo and fill in two settings:

   ```sh
   git clone https://github.com/itsfutsy/push.git
   cd push
   cp .env.example .env
   openssl rand -hex 32        # copy this: it's your upload token
   nano .env                   # set PUSH_DOMAIN and paste the token into PUSH_TOKEN
   ```

4. **Start it:**

   ```sh
   docker compose up -d
   ```

   The first start builds the image (a few minutes). After that, check it's alive:

   ```sh
   curl https://share.example.com/healthz     # ok
   ```

5. **Connect the app.** Open Push > Settings, enter `https://share.example.com` and paste the same token. When the
   status says **Connected**, you're done.

**Updating:** `git pull && docker compose up -d --build`

**Backups:** your files live in the Docker volume `push_push-data` (in a folder called `files`). Back it up with:

```sh
docker run --rm -v push_push-data:/data -v "$PWD":/backup alpine tar czf /backup/push-backup.tgz -C /data .
```

## Option 2: Your own reverse proxy or platform

Run the image built from `server/` behind whatever already terminates HTTPS on your server.

- **Build:** Dockerfile build with the build path (context) set to `/server`, Dockerfile `Dockerfile`.
- **Port:** the container listens on `3000`.
- **Storage:** mount a persistent volume at `/data`.
- **Environment:** see [Configuration](#configuration). Only `PUSH_TOKEN` and `PUSH_PUBLIC_URL` are required.
- **Health check:** built into the image, so platforms that respect it only switch traffic to healthy containers.

Proxy settings that matter:

- Allow request bodies of at least **10 MB**. The app uploads in 8 MB chunks, so huge files are fine.
- Keep the per-request **read timeout at 60 seconds or more** (Traefik's default of 60 s works down to about
  1.2 Mbit/s uploads). The app resumes after any dropped chunk.
- **Don't cache** responses. Deleting a file should make its link stop working at once.

If your platform writes a `.env` file with secrets next to the Dockerfile at build time (Dokploy and Coolify do),
that's fine: `server/.dockerignore` only lets `Cargo.toml`, `Cargo.lock` and `src` into the build.

## Option 3: Without Docker

```sh
cd server
cargo build --release
PUSH_TOKEN=... PUSH_PUBLIC_URL=https://share.example.com PUSH_DATA_DIR=/var/lib/push \
  ./target/release/push-server
```

Put it behind any HTTPS reverse proxy pointing at port 3000, and run it with systemd or similar.

## Configuration

| Variable | Required | What it does |
|---|---|---|
| `PUSH_TOKEN` | Yes | The upload token. At least 32 characters; generate one with `openssl rand -hex 32`. |
| `PUSH_PUBLIC_URL` | Yes | The address links start with, for example `https://share.example.com` (no trailing slash). The Compose file builds it from `PUSH_DOMAIN`. |
| `PUSH_ROOT_REDIRECT` | No | Where visitors to `https://share.example.com/` are sent, for example your homepage. Without it the root is a 404. |
| `PUSH_DATA_DIR` | No | Where files are stored. Default `/data`. |
| `PORT` | No | The port to listen on. Default `3000`. |

Treat the token like a password: anyone who has it can upload and delete. To change it, set a new one and restart,
then paste it into each app's Settings.

## Using Cloudflare

Cloudflare in front works well. With Option 1, start with the record on **DNS only** so Caddy can get its
certificate, then switch it to **Proxied** and set SSL/TLS mode to **Full (strict)**. Then:

1. **Caching > Cache Rules:** add a rule for your hostname with **Bypass cache**, so deletes take effect at once.
2. Turn these **off**, because they leak or break private links:
   - Caching > Configuration > **Always Online** (sends your URLs to the Internet Archive)
   - Caching > Configuration > **Crawler Hints** (sends your URLs to IndexNow)
   - Security > Bots > **Bot Fight Mode** (blocks link-preview bots like Discord's)
   - Scrape Shield > **Hotlink Protection** (breaks embeds)
   - Security > Bots > **Managed robots.txt** (replaces the server's own robots.txt)
3. If an old **Redirect Rule** sends your whole domain somewhere else, narrow it to `/` only or delete it, or every
   link will redirect too. `PUSH_ROOT_REDIRECT` does the root redirect for you.

The Free plan limits requests to 100 MB. Push never gets close, since it sends 8 MB chunks.

## Verify

```sh
TOKEN=<your token>
curl -i https://share.example.com/healthz                                       # 200 ok
curl -s https://share.example.com/robots.txt                                    # ends with "Disallow: /"
curl -si https://share.example.com/api/ping | head -1                           # 401
curl -si -H "Authorization: Bearer $TOKEN" https://share.example.com/api/ping | head -1   # 204
```

## Troubleshooting

| Symptom | Fix |
|---|---|
| Caddy can't get a certificate | The DNS record must point at this server and ports 80 and 443 must be open. With Cloudflare, use DNS only until the certificate exists. `docker compose logs caddy` says what went wrong. |
| Container restarts, log says `cannot write to /data/files: Permission denied` | The volume was created root-owned (usually a bind mount). Run `docker run --rm -v <volume>:/data alpine chown -R 65532:65532 /data`, then restart. |
| Build fails with `the --mount option requires BuildKit` | Install the buildx plugin (`apt install docker-buildx-plugin`). Docker from get.docker.com already has it. |
| Build is killed (out of memory) | A Rust release build needs 1 to 2 GB of RAM. Add swap. |
| Uploads on slow connections fail with 504 or resets | Your proxy's read timeout is too short. Raise it to 600 s (Traefik: `entryPoints.websecure.transport.respondingTimeouts.readTimeout`). |
| `413` on upload | A plain-text 413 from the server means a chunk over 10 MB; an HTML 413 comes from your proxy or Cloudflare, so raise its body limit. |
| `401` with the right token | Check `PUSH_TOKEN` for a missing or extra character, then restart. The server logs every rejected request. |
| Links open some other site | A redirect rule on your domain or proxy matches `/i/` too. See [Cloudflare](#using-cloudflare) step 3. |
