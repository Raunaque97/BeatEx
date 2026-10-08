# BeatEx web

The browser frontend for BeatEx. It uses Next.js App Router for the application
shell and keeps protocol behavior in `@beatex/sdk` and `@beatex/prover-wasm`.

```sh
cp apps/web/.env.example apps/web/.env.local
pnpm install
pnpm dev
```

The root `pnpm dev` command watches both local packages and starts Next.js.

## Boundaries

- Server Components may render static and public market data.
- Wallet access, private state, IndexedDB, Web Locks, and the WASM prover belong
  below a Client Component boundary.
- Proving keys and the WASM runtime are deployment artifacts under
  `/beatex-prover-assets/`; they are not committed to the application bundle.
- Private order openings must use the SDK storage interface. Do not put them in
  React Query, URL state, analytics, logs, or a general-purpose UI store.

See `docs/FRONTEND_ARCHITECTURE.md` for the full structure and dependency rules.
