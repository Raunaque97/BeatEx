# Vercel frontend with a local indexer

## Configured deployment

The named tunnel `beatex-indexer` on this Mac serves
`https://indexer.raun.cc`, forwarding to `http://127.0.0.1:8787`.
Its ID is `c6a63394-589c-439b-8cb9-aee47d775ac1`.

For Vercel Production and applicable Preview deployments, set:

```dotenv
BEATEX_INDEXER_URL=https://indexer.raun.cc
```

The tunnel configuration is `~/.cloudflared/beatex-indexer.yml`. A dedicated
LaunchAgent at `~/Library/LaunchAgents/com.beatex.indexer-tunnel.plist` starts it
at login and restarts it if the process exits. It uses IPv4 connections to
Cloudflare. The indexer itself still needs to keep running independently.

Verify or restart the tunnel on this Mac:

```sh
curl --fail https://indexer.raun.cc/health
cloudflared tunnel info beatex-indexer
launchctl kickstart -k "gui/$(id -u)/com.beatex.indexer-tunnel"
```

Logs are in `~/Library/Logs/beatex-indexer-tunnel.error.log` (cloudflared writes
its connection logs to stderr). Credentials stay outside the repository.

## How the proxy works

The browser requests `/api/indexer/v1/markets/<market>/auctions`. Next.js rewrites
that one endpoint to the server-only `BEATEX_INDEXER_URL`, preserving `limit`
and `before`. On Vercel this runs through its external-origin proxy without
invoking a Next.js function. Solana RPC reads and transactions still go directly
to the configured RPC.

Successful indexer history responses use `Cache-Control: public, max-age=0` and
`Vercel-CDN-Cache-Control: max-age=5, stale-while-revalidate=10`. Responses can be
served for five seconds fresh, then up to ten additional seconds while being
refreshed. Add the indexer's ingestion lag and the frontend's polling delay when
assessing chart freshness. Health checks and errors return `no-store`.

Older pages use the same short TTL because delayed indexing can fill gaps. The
client sends no cookies and uses consistent URLs to share cache entries across
viewers. The rewrite explicitly enables upstream caching for older Vercel
projects as well. See [Vercel external rewrites](https://vercel.com/docs/routing/rewrites).

## 1. Start the updated indexer

From the repository root, keep this process running:

If an indexer is already listening on port 8787, stop its existing process first
and restart it with this launcher to pick up the new caching headers.

```sh
bash scripts/devnet/run-indexer.sh --cluster devnet --listen 127.0.0.1:8787
```

The launcher builds the updated binary and uses the existing RPC configuration
under `.devnet/devnet`, or public Devnet RPC if none is configured. Keep the
existing state directory so history and the indexing cursor survive restarts.
The default HTTP listener is local to this machine.

In a second terminal:

```sh
curl --fail http://127.0.0.1:8787/health
```

## 2. Create a named Cloudflare Tunnel

Use a domain whose DNS is managed by Cloudflare. Replace
`indexer.example.com` with a subdomain you own. A named tunnel gives the Vercel
rewrite a stable origin. These commands follow the
[Cloudflare CLI setup guide](https://developers.cloudflare.com/tunnel/features/locally-managed-tunnels/create-local-tunnel/).

On macOS, install `cloudflared` if needed and start authentication:

```sh
brew install cloudflared
cloudflared tunnel login
```

Leave the login command running. In the browser window (or the newly printed
URL), sign in, select your Cloudflare-managed domain, and authorize the tunnel.
Wait for the terminal to report successful login and the saved `cert.pem` path
before proceeding; pressing Ctrl+C cancels the login. If no domains appear,
complete the domain/DNS prerequisite above first.

Then verify that the account certificate exists and create the tunnel:

```sh
test -f "$HOME/.cloudflared/cert.pem" && cloudflared tunnel create beatex-indexer
```

The create command prints a UUID and a credentials-file path. Put those values
in `~/.cloudflared/beatex-indexer.yml` (a separate file keeps existing tunnel
configurations intact):

```yaml
tunnel: YOUR-TUNNEL-UUID
credentials-file: /Users/YOUR-USERNAME/.cloudflared/YOUR-TUNNEL-UUID.json

ingress:
  - hostname: indexer.example.com
    service: http://127.0.0.1:8787
  - service: http_status:404
```

Then create the DNS route, validate, and run:

```sh
cloudflared tunnel route dns beatex-indexer indexer.example.com
cloudflared tunnel --config "$HOME/.cloudflared/beatex-indexer.yml" ingress validate
cloudflared tunnel --config "$HOME/.cloudflared/beatex-indexer.yml" run beatex-indexer
```

Keep both the tunnel and indexer running, using the same process supervision you
use for your bots. For automatic startup, Cloudflare also documents
[macOS service installation](https://developers.cloudflare.com/tunnel/features/locally-managed-tunnels/as-a-service/macos/).

Verify from another terminal:

```sh
curl --fail https://indexer.example.com/health
curl --fail -i 'https://indexer.example.com/v1/markets/8dE84BMbnqMEHjjJKZZaoS43B3dvA6sKGyquJdeCmX9v/auctions?limit=60'
```

The market above is the configured BEAT/tUSD Devnet example; substitute your
market if needed. An empty JSON array means the request worked but the indexer
has no history for that market yet.

Use Vercel as the cache layer initially; no Cloudflare JSON Cache Rule is needed.
An interactive Cloudflare Access login would block the Vercel proxy. The tunnel
hostname currently serves public, read-only history and health; direct requests
to it bypass the Vercel cache. Restricting it to authenticated Vercel requests
would require an additional origin-authentication configuration.

## 3. Configure Vercel before building

Import this repository with:

| Setting                                     | Value                                     |
| ------------------------------------------- | ----------------------------------------- |
| Framework                                   | Next.js                                   |
| Root Directory                              | `apps/web`                                |
| Include source files outside Root Directory | Enabled (workspace packages are required) |
| Node.js                                     | 22.x                                      |
| Install Command                             | `pnpm install --frozen-lockfile`          |
| Build Command                               | `cd ../.. && pnpm build:vercel`           |
| Output Directory                            | Framework default (`.next`)               |

Add this environment variable to Production and any Preview deployments that
should use the indexer:

```dotenv
BEATEX_INDEXER_URL=https://indexer.example.com
```

Use the origin only, without `/v1` or `/api/indexer`. Remove the obsolete
`NEXT_PUBLIC_BEATEX_INDEXER_URL`. The build fails on Vercel if the origin is
missing or isn't HTTPS. The rewrite is built at deployment time: redeploy after
changing this value. Keep the existing public cluster, RPC, program, market, and
prover-manifest settings from `apps/web/.env.example` as appropriate.

`apps/web/vercel.json` supplies the install and build commands. The
`build:vercel` script downloads the matching WASM runtime, four proving keys,
and manifest from the public GitHub Release pinned in
`apps/web/prover-assets.release.json`. It verifies the manifest's pinned SHA-256
and every binary's digest and length before replacing the public asset directory.
No GitHub token or Rust toolchain is needed during the Vercel build.

Vercel then serves the files at `/beatex-prover-assets/` from the frontend's
origin, so keep `NEXT_PUBLIC_BEATEX_PROVER_MANIFEST_URL` at its default
`/beatex-prover-assets/manifest.json`. Browser integrity checks and Cache Storage
still apply. The asset directory remains Git-ignored.

The source repository is [Raunaque97/BeatEx](https://github.com/Raunaque97/BeatEx).
The current asset release is
[prover-assets-2d3c8d70989d](https://github.com/Raunaque97/BeatEx/releases/tag/prover-assets-2d3c8d70989d).
When regenerating assets, publish a new release and update both the manifest URL
and digest in the release configuration; do not overwrite the pinned release.

## 4. Verify the deployed proxy and cache

Run the same request twice, close together, against your deployment:

```sh
curl --fail -sS -D - -o /dev/null 'https://YOUR-APP.vercel.app/api/indexer/v1/markets/8dE84BMbnqMEHjjJKZZaoS43B3dvA6sKGyquJdeCmX9v/auctions?limit=60'
curl --fail -sS -D - -o /dev/null 'https://YOUR-APP.vercel.app/api/indexer/v1/markets/8dE84BMbnqMEHjjJKZZaoS43B3dvA6sKGyquJdeCmX9v/auctions?limit=60'
```

Expect HTTP 200 and a warmed `x-vercel-cache: HIT`; the first request may already
hit a warm cache. Regional caches and eviction can cause additional misses. CDN
cache hits are verified on Vercel, not through local `next dev` or `next start`.
An upstream failure should remain uncached. In browser developer tools, history
requests should use the frontend origin and `/api/indexer/...`.

For local development, the origin defaults to `http://127.0.0.1:8787`; an existing
`.env.local` can omit `BEATEX_INDEXER_URL` or set it explicitly. Restart Next.js
after changing it.
