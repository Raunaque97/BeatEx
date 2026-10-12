# `@beatex/sdk`

Browser-compatible TypeScript SDK for the BeatEx Solana program.

The SDK owns protocol account decoding, PDA derivation, Anchor-compatible
instruction encoding, Solana legacy and extended-v1 transaction construction, proof orchestration,
and crash-safe private-state transitions. Frontends should use this package
instead of constructing BeatEx instructions directly.

## Client

```ts
import {
  BrowserPrivateStateStorage,
  createBeatExClient,
  createBeatExRpc,
  createKitSignerAdapter,
} from "@beatex/sdk";

const beatex = createBeatExClient({
  rpc: createBeatExRpc("https://api.devnet.solana.com"),
  signer: createKitSignerAdapter(walletSigner),
  prover,
  storage: new BrowserPrivateStateStorage(localStorage),
  cluster: "devnet",
});

await beatex.deposit({
  market,
  baseSource,
  quoteSource,
  baseAmount: 100n,
  quoteAmount: 10_000_000n,
});

await beatex.placeOrder({
  market,
  curve: {
    // Both sides are base-token quantities. One level cannot contain both.
    buy:  [0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 8n, 6n, 4n, 2n, 0n, 0n, 0n, 0n],
    sell: [4n, 3n, 2n, 1n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n, 0n],
  },
});

await beatex.replaceOrder({ market, curve: replacement });
await beatex.cancelOrder({ market });
await beatex.settleToBalance({ market, auctionId });
await beatex.refundToBalance({ market, auctionId });
await beatex.rollover({ market, previousAuctionId: auctionId, nextCurve });
await beatex.withdraw({
  market,
  baseDestination,
  quoteDestination,
  baseAmount: 1n,
  quoteAmount: 1_000_000n,
});
```

All token and price values use integer on-chain units and are represented as
`bigint`. `buy[i]` and `sell[i]` are always base-token quantities at grid index
`i`; the quote liability of a buy is `price[i] * buy[i]`.

## Proving boundary

The SDK does not assume where proofs run. `BeatExProver` can be implemented by
a browser worker using WASM, a native bridge, or an authenticated proving
service. Every method receives the complete typed witness request and must
return the exact proof, ciphertext transition, and next private opening.

The SDK validates the result's fixed-width wire shape, reconstructs the
program instruction, simulates it, and asks the wallet to sign the resulting
transaction. Private-balance initialization, deposits, and withdrawals use the
widely supported legacy envelope. Orders and settlements retain Solana's
extended v1 envelope because their payloads exceed the legacy size limit.

## Private state

Private balance openings, order quantities, randomness, and order salts cannot
be recovered from Solana. A frontend must back up its `PrivateStateStorage`.
The provided storage implementations use a two-phase journal:

1. Compile and simulate before requesting a wallet signature; simulate the
   returned transaction again because a wallet may modify its message.
2. Persist the next private opening as pending.
3. Broadcast and record the transaction signature.
4. Commit the opening only after confirmation.

If a page reload interrupts confirmation, call
`recoverPendingTransition(market)`. The SDK refuses a new mutation while a
transition is unresolved.

`BrowserPrivateStateStorage` provides atomic cross-tab updates through Web
Locks, but it does not encrypt values by itself. Applications should supply an
encrypted `WebStorageLike` implementation or their own encrypted
`PrivateStateStorage` before handling valuable assets.

## Read API

The client exposes:

- `getMarket`
- `getCurrentAuctionId`
- `getAuction`
- `getAuctionPhase`
- `getQueueProgress`
- `getPrivateBalance`
- `getPrivateState`
- `getActiveOrder`
- `getQueuedOrder`
- `initializeCurrentAuction`

The program stores only `Pending`, `Settled`, and `Cancelled`; the SDK derives
the user-facing collecting, aggregating, publishing, and expired phases from
the current slot and the auction deadlines.

## Transaction constraints

The package builds extended Solana version-1 transactions and rejects anything
larger than 4,096 bytes before broadcast. Rollover preserves the exact required
six-instruction order and does not append optional instructions because the
live transaction has only three bytes of headroom.

Default limits match the deployed client: 1.4 million compute units, 256 KiB
heap, 2 MiB loaded account data, and zero priority fee. They may be overridden
in `transactionConfig`.
