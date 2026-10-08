# Frontend architecture

## Stack

| Concern | Choice | Reason |
| --- | --- | --- |
| Application | Next.js 16 App Router + React 19 + strict TypeScript | Current framework baseline, typed routes, and a clean server/client boundary |
| Styling | Tailwind CSS 4 + CSS design tokens | Fast iteration without hiding protocol UI behind a component framework |
| UI primitives | shadcn/ui, added component-by-component | Accessible source-owned primitives without a runtime UI dependency |
| Solana | `@solana/kit`, `@solana/react`, and `@solana/kit-plugin-wallet` | Wallet Standard discovery and alignment with the existing SDK |
| Protocol | `@beatex/sdk` | The only owner of instructions, account decoding, transaction compilation, and private-state workflows |
| Proving | `@beatex/prover-wasm` | Lazy proving assets and a long-lived dedicated Web Worker |
| Remote state | TanStack Query | RPC caching, invalidation, polling, and transaction-driven refreshes |
| Validation | Zod | Environment and boundary validation; proof witnesses remain typed by the SDK |
| Unit tests | Vitest + Testing Library | Fast component, hook, and state-machine tests |
| Browser tests | Playwright (add with the first end-to-end flow) | Wallet and devnet/local-validator user journeys |
| Workspace | pnpm workspaces | Links the frontend, SDK, and prover without publishing intermediate packages |

Do not add Redux, Zustand, a second Solana client library, or a chart library
until a feature demonstrates the need. Local component state is enough for the
first order editor; TanStack Query owns public remote state. A small UI store can
be added later for ephemeral panel/layout state only.

## Repository shape

```text
apps/web/
  public/                         immutable public assets; generated prover assets are deployed here
  src/
    app/                          routes, layouts, loading/error boundaries; keep these thin
      trade/[market]/             market trading route
    components/
      layout/                     application chrome
      ui/                         source-owned shadcn primitives as they are introduced
    features/
      markets/                    market discovery and market metadata
      auctions/                   phase/timer, clearing history, aggregate curves
      orders/                     encrypted order editor, replace/cancel, proof progress
      balances/                   deposit, private balances, settle, withdraw
      portfolio/                  positions and activity
      trading/                    composition of the trading workspace
    lib/
      beatex/                     browser-only SDK/prover assembly
      config/                     validated public configuration
      query/                      query client and query-key factories
      solana/                     wallet and cluster adapters
    providers/                    narrow client-side providers
    test/                         shared test fixtures
packages/sdk/                     protocol TypeScript SDK
packages/prover-wasm/             browser prover and worker bridge
```

Create feature subdirectories when their first real file is added; do not commit
empty directory scaffolding. A route imports a feature composition component,
and feature modules import shared `components`, `lib`, and the SDK. Shared code
must not import from a feature, and one feature must not reach into another
feature's internals.

## Runtime boundaries

The root layout and public/static pages remain Server Components. The interactive
trading workspace introduces a Client Component boundary at the narrowest point
that needs wallet hooks. The BeatEx client is constructed after wallet connection
from:

1. validated cluster/RPC configuration;
2. the Wallet Standard signer adapted to the SDK signer boundary;
3. an encrypted IndexedDB implementation of `PrivateStateStorage`;
4. a lazily constructed `WasmBeatExProver`.

The prover runtime and keys are fetched only when a proof is first requested.
Their manifest digest and byte lengths are checked by `@beatex/prover-wasm`.
Proof progress is UI state; witnesses and private order openings are not.

## Query and transaction rules

- Query keys start with `['beatex', cluster, programId, market]` so switching a
  cluster or market cannot reuse incompatible cached data.
- Account queries use explicit commitment and expose the observed slot.
- A submitted transaction invalidates only the affected market, auction,
  balance, and order keys after confirmation.
- Auction timing is derived from on-chain slots. Browser time is display-only.
- Every transaction screen shows `building`, `proving`, `awaiting-signature`,
  `submitted`, `confirmed`, and `failed` separately.
- Proof work remains cancellable at the UI boundary, with the WASM worker's
  documented cooperative-cancellation limitation.

## Security and privacy

- Never log witnesses, openings, randomizers, plaintext curves, or private
  balances.
- Never place private data in URLs, React Query, analytics, crash metadata, or
  unencrypted local/session storage.
- The current SDK browser storage is suitable for development. Before public
  use, provide an encrypted IndexedDB adapter with versioning, migration, and
  export/recovery behavior.
- Host the app and proof assets on HTTPS. Add a strict CSP once wallet and proof
  asset origins are known.
- Cross-origin isolation headers are intentionally deferred. They become a
  deployment requirement if the prover moves to threaded WASM, and must be
  tested against wallet flows and every external asset first.

## Delivery order

1. Wallet/cluster provider and BeatEx client factory.
2. Read-only market and auction state with slot-derived phase/timer.
3. Private deposit and balance dashboard.
4. Order editor, proof progress, submit/replace/cancel.
5. Settlement, rollover, and withdrawal flows.
6. Aggregate curves/history and activity views.
7. Playwright wallet flows against local validation, then Devnet.
