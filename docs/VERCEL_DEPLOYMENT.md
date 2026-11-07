# Vercel frontend with a local indexer

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

## 5. Run the demo without market makers

Standing market-maker bots aren't needed for the demo. While a trader has an
order drafted, or a limit order working, the page calls `POST /api/maker` once per
auction. The route quotes the next auction from a throwaway wallet derived from
`FAUCET_SECRET_KEY`, the market, and the auction ID. The wallet lives for one
function call, so no private state is stored: it receives freshly minted tokens
and a loan of SOL, deposits, places a two-sided order, settles once the auction
finishes, and returns the remaining SOL. A maker takes longer to prepare than the
16-second window, so it has to start before the trader clicks. An order always
goes into the running auction: if no maker reached that auction, it comes back
unfilled and the page says there was nothing to match.

The route also opens any auction a trader or maker needs, paying about 0.0376 SOL
of rent from the faucet wallet. Finishing the auction returns that rent to the
market authority, not the faucet, because the program fixes the receiver. Start
one on-demand committee with `--sweep-to <FAUCET_ADDRESS>` to send the authority's
SOL above a 0.25 SOL reserve back each minute; only one needs it, as the
committees share that wallet. Beyond that round trip, each auction with a maker
costs about 0.004 SOL, the page stops asking after a minute without input or when
its tab is hidden, and an idle market costs nothing.
One maker runs per auction however many traders ask.

`FAUCET_SECRET_KEY` must be the mint authority of the market's tokens, because
both the faucet and the maker mint rather than hold stock. The optional sizing
settings are in `apps/web/.env.example`. The route needs the default 300-second
function duration, and it fetches the prover assets from its own origin, so
previews behind Deployment Protection can't run it.

Only the committee stays running, one per market. It must use `--on-demand` so it
never opens auctions. It then polls every two seconds and sends nothing while
idle:

```sh
scripts/devnet/run-committee-bot.sh --on-demand \
  --wallet .devnet/devnet/devnet-wallet.json
```

Auction rent is paid by the faucet wallet and refunded to the market authority,
which is also the committee's fee payer. Move SOL back to the faucet wallet when
it runs low.

The setup scripts mint with `--wallet`, so re-running their mint steps
(`devnetSetup.sh`, `setup-test-tokens.sh`, `setup-beat-market.sh`) fails unless
that wallet is the mint authority. Use the faucet wallet for those steps, or move
the authority back with `spl-token authorize`.

