# BeatEx web

The browser frontend for BeatEx. It uses Next.js App Router for the application
shell and keeps protocol behavior in `@beatex/sdk` and `@beatex/prover-wasm`.

```sh
cp apps/web/.env.example apps/web/.env.local
pnpm install
pnpm dev
```

The root `pnpm dev` command watches both local packages and starts Next.js.

Auction history is fetched through `/api/indexer/v1/markets/<market>/auctions`.
Set the server-only `BEATEX_INDEXER_URL` to the indexer origin; it defaults to
`http://127.0.0.1:8787` locally. The old `NEXT_PUBLIC_BEATEX_INDEXER_URL` is no
longer used. Changes to the origin require restarting Next.js or redeploying.

See [the Vercel deployment guide](../../docs/VERCEL_DEPLOYMENT.md) for Cloudflare
Tunnel commands, Vercel build settings, and cache verification.

`pnpm build:vercel` at the repository root downloads and verifies the pinned
GitHub Release's prover assets, then builds the workspace packages and frontend.
The deployment serves the WASM and proving keys from `/beatex-prover-assets/`.
Use `pnpm assets:web` to fetch just the assets during local setup.

## Boundaries

- Server Components may render static and public market data.
- Wallet access, private state, IndexedDB, Web Locks, and the WASM prover belong
  below a Client Component boundary.
- Proving keys and the WASM runtime are deployment artifacts under
  `/beatex-prover-assets/`; they are not committed to the application bundle.
- Private order openings must use the SDK storage interface. Do not put them in
  React Query, URL state, analytics, logs, or a general-purpose UI store.

See `docs/FRONTEND_ARCHITECTURE.md` for the full structure and dependency rules.
