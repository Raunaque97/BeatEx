# BeatEx

Confidential frequent batch auctions on Solana.

The repository now contains the Anchor program, Baby Jubjub order/fold,
deposit, aggregate-publication, settlement, refund, and withdrawal Groth16
circuits, Token-2022 custody integration, LiteSVM/SBF benchmarks, and local
test-token/deployment tooling described in
[`TECHNICAL_DESIGN.md`](./TECHNICAL_DESIGN.md).

The default account model is deposit once, trade repeatedly, and withdraw on
demand. Public Token-2022 deposits are credited to a private Baby Jubjub
balance. Settlements and cancellation refunds return value to that encrypted
balance without token transfers. A later public withdrawal proves the exact
encrypted-balance debit and transfers both assets from the market vaults in one
transaction. Individual orders, fills, proceeds, and running balances remain
private; deposits and public withdrawal amounts are visible. The current
balance is scoped to one market pair and key epoch. A future shared asset pool
would require common vault-authority and encryption-key domains across markets;
it is not implied by this release.

## Auction FAQ

### Can one wallet submit more than one order in an auction?

The program keeps one private `ActiveOrder` per `(auction, owner)`. A later
submission uses `replace_queued_order`: a collateral-release proof restores the
previous reserve, the ordinary order proof locks the replacement reserve, and
one immutable queue event records the encrypted transition `new_order -
old_order`. `cancel_active_order` proves the same reserve release and records
`-old_order`. Both operations are atomic single transactions. Folding
all accepted delta events therefore leaves exactly the latest order in the
aggregate while still allowing bots to fold during collection. The final active
revision, rather than every historical event, will be claimable. Observers can
see that an authority submitted, changed, or cancelled an order, but not its
private quantities.

Each replacement stores a pointer to the immutable prior event rather than
duplicating its ciphertext curve. Fold bots follow that pointer for the private
subtraction witness, while the onchain event digest remains bound to both
curves.

This preserves ordinary cancel/replace UX. It also prevents one signing
authority from leaving simultaneous opposite orders in the auction, although a
permissionless market cannot prevent the same person from using unrelated
wallets.

### Can a recurring trader claim auction N and place auction N+1's order atomically?

Yes. One version-1 user transaction carries six BeatEx instructions:
`prepare_rollover`, settlement-proof verification, settlement-statement
binding, order-proof verification, order-statement binding, and the final
`rollover_order` mutation. The first proof credits auction N's private settled
payout directly into the trader's Baby Jubjub encrypted balance; the ordinary
order proof immediately deducts auction N+1's reserves. The final instruction
terminally claims the old order and creates the new queue event and active-order
pointer atomically, with no Token-2022 transfer proof contexts.

The preparation and binding instructions inspect the instructions sysvar to
require the exact proof/binding/mutation sequence and account mapping in the
same transaction. They do not leave reusable proof receipts or partially
prepared rollover state: if any instruction fails, Solana rolls the complete
transaction back. Splitting the work gives each Groth16 verification and each
statement-hash reconstruction a fresh SBF heap while preserving one-signature,
one-transaction UX. The two proofs are independent once their intermediate
ciphertexts are chosen, so clients can generate them concurrently. Rollover is
limited to auctions using the same key epoch; key rotation uses the ordinary
claim and deposit path.

### How is an auction's reference price selected?

The version-2 policy centers auction N+1 on auction N-1's clearing price: while
auction N collects orders, the committee decrypts and settles N-1. If N-1 (or a
later auction) has cleared with volume at least the configured minimum, its
price is used; otherwise a fresh Pyth BTC/USD price is used. The first auctions
have no cleared predecessor and therefore use Pyth.

Initialization timing enforces this. Once N-1 has cleared, N+1 may be
initialized up to 12 slots before it opens so traders can prepare orders
against its grid. Until then, initialization is accepted only in the last 2
slots before opening (or during the auction's own window), so a permissionless
caller cannot lock in the oracle price while N-1 is still settling. For the 8-decimal tBTC / 6-decimal tUSD market, one base quantity unit is
100 satoshis and one quote accounting unit is one micro-tUSD. The oracle
exponent adjustment is therefore `0`, and an integer protocol price of `85_500`
means approximately 85,500 tUSD per tBTC. A stale update, wrong feed, excessive
confidence interval, or arithmetic overflow makes initialization fail closed.
The auction records the chosen reference price and its source.

A market can also be created without an oracle (`initial_reference_price` in
`initialize_market`, no oracle account). Its reference price is always its last
clearing price, starting at the initial price; zero-volume auctions keep the
last price, so the price moves only when trades clear. The devnet BEAT/tUSD
market works this way, starting at 1 tUSD per BEAT.

### How are the 17 price levels defined?

The market fixes `grid_radius = 8` and an immutable percentage step, defaulting
to `grid_step_bps = 10`. Each auction derives 17 **linear** offsets from one
reference price: `-80, -70, ... 0, ... +70, +80` basis points. These are not
recursively compounded percentages.

For index `i` from 0 through 16:

```text
offset = i - 8
price[i] = floor(
  (reference_price * (10_000 + offset * grid_step_bps) + 5_000) / 10_000
)
```

The formula uses checked integer arithmetic and rounds to the nearest quote
unit, with exact halves upward. Initialization rejects overflow and any rounded
grid with duplicate or non-increasing levels. The Auction account snapshots the
reference price, basis-point step, and level count; every client derives the
same exact 17 prices from those immutable fields. That account—not a local
oracle fetch or command-line argument—is authoritative for the SDK, clients,
proofs, and clearing.

Order `buy[i]` and `sell[i]` values are counts of 100-satoshi base units. The
minimum order increment is therefore 100 satoshis; the version-2 proof permits
28-bit quantities, while each market may configure a lower maximum. Applications
accept normal token amounts and perform these integer conversions for users.

Pyth already runs its receiver on Solana Devnet, and its sponsored shard-zero
BTC/USD feed is available there. BeatEx therefore does not need to deploy a test
oracle for tBTC/tUSD. The Devnet account is
`4cSM2e6rvbGQUFiJbqytoVMi5GgghSMr8LwVrT9VPSPo`, for feed ID
`e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43`.

### What auction state does the program actually store?

`Auction.state` stores only `Pending`, `Settled`, or `Cancelled`. The market
authority configures and starts the market once; it does not open, seal, or
clear individual auctions. The current auction ID and every time window are
derived from the onchain slot.

- `initialize_auction` is permissionless and creates the PDA, counter, and
  accumulator for exactly the slot-derived auction ID. It snapshots the active
  key epoch and either the eligible prior clearing price or the configured Pyth
  price.
- While `Pending`, slot checks admit orders only during collection and folds
  only through the aggregation deadline.
- `publish_and_clear` is permissionless. After collection and once accepted and
  folded counters match, it verifies the committee-produced Groth16 proof,
  publishes both aggregate curves, deterministically clears, and atomically
  changes `Pending -> Settled`.
- `cancel_pending_auction` is permissionless and changes `Pending -> Cancelled`
  after an incomplete-aggregation or publication timeout.

Claims and cancellation refunds are terminal per-order transitions; they do
not add another auction phase. Settled claims remain available until consumed.
A failed instruction is atomic and leaves the state unchanged.

## Implementation roadmap

There are seven phases, numbered 0 through 6:

- **Phase 0 — Feasibility gates and queue architecture complete**
  - [x] Offchain Solana-compatible 3-of-7 threshold-decryption harness
  - [x] Anchor 1.1.2 program scaffold and public key-epoch account
  - [x] Program-side Ristretto/DLEQ verification and 3-of-7 share combination
  - [x] SBF syscall execution and compute-unit benchmarks
  - [x] Versioned Groth16/BN254 verifier pipeline in SBF
  - [x] Exact Ristretto ciphertext-binding circuit and constraint measurement
  - [x] Benchmark native grouped-ciphertext validity and batched range proofs
  - [x] Implement and benchmark the real 17-level Baby Jubjub/Groth16 relation
  - [x] Reject direct Baby Jubjub aggregation from measured SBF CU
  - [x] Reject BN254 G1 ElGamal from measured Groth16 constraint cost
  - [x] Implement independent queued-order/balance proofs and permissionless fold proofs
  - [x] Implement Anchor accepted/folded counters, order PDAs, aggregate PDA, and timeout gate
  - [x] Shared-vault pending-balance and atomic payout/refund CPI code
  - [x] Token-2022 v11 proof-context CPI integration test in LiteSVM
- **Phase 1 — Plaintext auction core (complete in host and SBF tests)**
- **Phase 2 — Encrypted queue and proof-backed folding (Anchor + host proofs complete; clean SBF build complete)**
- **Phase 3 — Committee decryption (Baby Jubjub publication circuit and SBF verification complete)**
- **Phase 4 — Custody and encrypted trading balances (complete)**
- **Phase 5 — Private settlement, cancellation fallback, and public withdrawal (complete in host and SBF tests)**
- **Phase 6 — Devnet integration (deployed; full fast-ledger lifecycle verified live)**

The completed Phase 0 checkpoint currently provides:

- dealer-assisted Shamir setup around Solana's twisted ElGamal secret scalar;
- public verification of the dealer setup;
- Chaum-Pedersen proofs for decryption shares;
- Lagrange combination of any valid threshold subset;
- verification of caller-supplied aggregate plaintexts;
- byte-level ciphertext round trips through Solana's SDK types;
- an Anchor instruction that verifies three context-bound decryption shares and
  the resulting plaintext using Solana's native Ristretto APIs;
- a deterministic Groth16 fixture proving that BN254 proof serialization and
  Solana verification work end to end;
- a host-side three-level Groth16 order-arithmetic spike that enforces bounded
  quantities and per-level buy/sell exclusivity (without yet claiming
  Ristretto ciphertext binding);
- an exact Arkworks BN254 circuit for Solana's compressed Ristretto
  twisted-ElGamal relation, with positive and ciphertext-mutation tests;
- real Token-2022 confidential-transfer and apply-pending CPI construction;
- a 26,885-constraint deposit circuit that atomically links public Token-2022
  deposits to the trader's Baby Jubjub encrypted private balance;
- a 518,852-constraint aggregate-publication circuit for any sorted 3-of-7
  Baby Jubjub quorum, with private committee-share witnesses and SBF verification;
- 2,924,068-constraint claim and 2,923,244-constraint refund circuits that
  reopen the private order commitment, enforce exact-index allocation, and bind
  two Token-2022 payouts through one exact batched Ristretto relation;
- permissionless claim/refund relay handlers plus a threshold vault-proof
  library that produces host-verified standard Token-2022 proof bundles while
  retaining the private Pedersen opening needed by the claim prover, creates
  the six proof-context transaction plans, and assembles the exact
  program-bound claim/refund witness;
- a full production SBF claim using the 2.9-million-constraint Groth16 proof
  and two Token-2022 confidential-transfer CPIs at 265,954 CU;
- current-SDK Token-2022 proof-context generation using Solana ZK SDK v7;
  v4 proofs are rejected because the current native proof program applies the
  newer global transcript domain separator;
- rejected native two-handle grouped-ciphertext-validity and range-proof
  benchmarks in Rust and browser WebAssembly (the selected Groth16 path uses
  the 28-bit order bound);
- a clean default-stack SBF build with a measured monolithic grid cap of 17
  price levels (19 is the first failing account layout);
- full production-SBF execution of a 17-level order submission at 514,651 CU,
  a replacement-aware three-order fold at 614,480 CU, and an active-order
  cancellation at 419,817 CU.

Run the checks with:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Run the current encrypted multi-trader simulation with:

```sh
cargo run -p beatex-auction-sim
```

It encrypts three private order curves, homomorphically aggregates every grid
entry, verifies participant-specific 3-of-7 decryption-share proofs, publishes
only the aggregate curves, and applies the deterministic clearing rule.

The ordinary settlement path uses the 31,880-constraint encrypted-balance
relation and takes about 0.53 seconds per proof on an Apple M4. The public
withdrawal reuses the 26,885-constraint deposit relation in reverse and its
release test, including deterministic setup, completes in under 0.7 seconds.
Both are single user transactions. The exact Token-2022 confidential-claim
prover is an optional committee-side path, not the normal trading path. On
an Apple M4 its Arkworks release benchmark takes 40.833 seconds per proof after
the one-time setup; host verification is 1.331 ms. The circuit is deliberately
retained for confidential Token-2022 exits. The direct encrypted-balance
rollover proof and the next order proof can run concurrently.

The Devnet environment uses named Token-2022 test assets `tBTC` and `tUSD`.
The idempotent local-validator/Devnet mint workflow and current toolchain
requirements are documented in
[`docs/DEVNET_ENVIRONMENT.md`](./docs/DEVNET_ENVIRONMENT.md).

The current 972,376-byte SBF program is deployed on Devnet at
`2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd`; its SHA-256 digest is
`e8d377290afe956253ba747375977c8ba2e0392239bb8121443085c70725a3e7`.
The retained Devnet tBTC and tUSD addresses are recorded in
`docs/DEVNET_ENVIRONMENT.md`.

Frontend applications use the browser-compatible TypeScript package at
[`packages/sdk`](./packages/sdk/). It provides typed deposit, order,
replacement, cancellation, internal settlement/refund, rollover, withdrawal,
read, transaction, prover, and crash-safe private-state APIs. The frontend does
not need to encode Anchor instructions or manage balance revisions directly.

Trader proof generation is implemented by the companion package at
[`packages/prover-wasm`](./packages/prover-wasm/). One Web Worker owns a single
WASM runtime and lazily loads only the balance, order, release, and settlement
proving keys needed by the current action. Folding and aggregate publication
remain infrastructure provers and are not shipped to traders.

### Native client and Phantom signing

The Rust `beatex` client runs proof generation natively and uses Phantom only
for wallet ownership and transaction approval. Build the native client and the
underlying Devnet executor together:

```sh
cargo build -p beatex-cli -p beatex-devnet-client
```

Connect once to save the public wallet profile:

```sh
target/debug/beatex wallet connect
target/debug/beatex wallet status
```

The client opens a page served only on a random `127.0.0.1` port. Phantom signs
a short-lived, origin-bound login challenge; the client verifies the Ed25519
signature and saves only the wallet name, public key, and advertised Solana
transaction versions. The session bearer token stays in the URL fragment and
is never persisted. The local page has no remote scripts.

Open the terminal interface with `target/debug/beatex tui`. The current screen
shows wallet status, the live Pyth BTC/USD price, a rolling Ratatui line chart,
and the intended trading actions; interactive forms are still being connected.
The price is read from the configured Pyth account on Solana Devnet every two seconds. Override the
defaults with `BEATEX_RPC_URL`, `BEATEX_RPC_FALLBACK_URLS`,
`BEATEX_PYTH_PRICE_ACCOUNT`, and `BEATEX_PYTH_FEED_ID` when needed. Every
existing native Devnet operation is already
available through the browser-wallet wrapper. Put the ordinary
`beatex-devnet-client` arguments after `--` and omit `--wallet`; for example:

```sh
target/debug/beatex devnet -- order \
  --base-mint BASE_MINT \
  --quote-mint QUOTE_MINT \
  --base-source BASE_TOKEN_ACCOUNT \
  --quote-source QUOTE_TOKEN_ACCOUNT \
  --state-dir .devnet/state \
  < order.json
```

For each execution, the client opens a fresh local signing session and checks
that Phantom selected the saved public key. The browser page refuses order and
rollover signing unless the installed wallet advertises transaction version 1.
The Rust prover prepares the proof-heavy instruction data, the existing sender
compiles the 4,096-byte-capable v1 transaction, and Phantom signs those exact
bytes. Returned transactions are rejected if the wallet changed the compiled
message. The ephemeral session descriptor is deleted when the command exits.

Create or reuse the test assets, deploy the program, initialize the custody
vaults, 3-of-7 key epoch and all eight versioned verification-key PDAs with one
command:

```sh
scripts/devnet/devnetSetup.sh --wallet /absolute/path/to/devnet-wallet.json
```

Before deployment, the setup script derives the configured BeatEx program ID
and checks it on the selected cluster. A first deployment proceeds normally;
an existing deployment is displayed and must be approved before it is
upgraded. Declining keeps the existing program. In a non-interactive shell,
the safe default is also to keep it; pass `--redeploy` only after explicitly
authorizing the upgrade. The lower-level `deploy-beatex.sh` enforces the same
guard when it is run directly.

The setup then rebuilds `target/idl/beatex.json` and publishes it
as the program's canonical IDL metadata. Existing metadata is fetched first:
identical JSON is skipped, while a changed IDL requires confirmation. Use
`--skip-idl` to omit this step or `--allow-idl-update` for an already-approved,
non-interactive metadata update.

A separate permissionless coordinator handles auction initialization, queue
folding, and aggregate publication:

```sh
scripts/devnet/run-committee-bot.sh \
  --wallet .devnet/devnet/devnet-wallet.json
```

The coordinator can be run by any funded fee payer. PDA and counter checks make
repeated initialization, folding, and publication attempts safe; expected
errors while an auction is still collecting are retried.

The reusable `OrderProver` API accepts arbitrary 17-level curves, a canonical
committee key, and the trader's private encrypted-balance opening. It returns
all 34 ciphertexts, the updated encrypted balance and opening, order commitment,
reserve amounts, and a self-verified Solana-format Groth16 proof.
`ReplacementProver` atomically chains collateral release into that order proof,
while the rollover prover chains the exact settled payout into the next
auction's order. Cancellation uses the standalone `CollateralReleaseProver`.
These APIs reject non-canonical points/scalars and private state that does not
reopen the onchain order commitment.
`DepositProver` performs the preceding public-token-to-encrypted-balance
transition for arbitrary amounts and openings. `AggregatePublicationProver`
accepts a live folded aggregate and committee quorum instead of a fixed fixture.
The generic [`clients/v1-transaction`](./clients/v1-transaction/) sender handles
the 4,096-byte version-1 signing and confirmation layer, including multiple
instructions and additional signers. The stateful adapter in
`crates/devnet-client` also exposes permissionless `fold`, `publish`, and
`cancel` commands. `settle-balance` and `refund-balance` credit the encrypted
ledger, and `withdraw` proves a ledger debit and transfers public Token-2022
funds. The optional `claim` and `refund` commands retain the slower confidential
Token-2022 bridge. The adapter also assembles the six-instruction atomic
rollover transaction used by recurring traders.

### Build and deployment pipeline

The Anchor program is the source of truth for the program ID, protocol units,
grid parameters, circuit versions, and IDL. Run the pipeline from the repository
root after changing `programs/beatex`:

```sh
# SBF + Anchor IDL + generated SDK/Rust protocol artifacts
pnpm build:contract

# Above plus workspace checks and SDK/prover package builds
pnpm build:protocol

# Full build, including trader WASM assets and the web application
pnpm build:all
```

`scripts/build-pipeline.sh` runs
`scripts/generate-protocol-artifacts.mjs` automatically. That generator updates
the frontend SDK constants at
`packages/sdk/src/protocol/generated.ts`, the Rust prover constants at
`crates/babyjubjub-groth16-spike/src/generated_protocol.rs`, and the deployment
manifest at `.devnet/<cluster>/deployment.json`. The web app imports its default
program ID from the SDK, so it does not carry a second manually-maintained
address.

`scripts/devnet/deploy-beatex.sh` invokes the contract pipeline before every
non-skipped build. `scripts/devnet/devnetSetup.sh` invokes the generator again
after token, market, committee, and verification-key initialization, so the
new market and vault addresses are captured in the same manifest. Consumers
such as the CLI and devnet runners read the state manifests; no frontend,
prover, CLI, TUI, or bot source file needs to be edited after a redeployment.
The scripts still require explicit approval before upgrading an existing
program; this pipeline does not send a deployment transaction by itself.

The default fast lifecycle was executed live in auction 1007. Deposit, order,
fold, aggregate publication, internal settlement, and public withdrawal all
finalized with zero priority fees. The internal settlement used 154,806 CU;
the two-asset public withdrawal used 134,005 CU in a 1,031-byte transaction.

The first live Devnet private claim settled auction 18 at 267,107 CU in a
1,510-byte v1 transaction. The timeout-refund run cancelled auction 14
permissionlessly and executed its refund at 267,105 CU in 1,510 bytes. Each
proof-context transaction paid 10,000 lamports because it has two signatures;
the withdrawal and cleanup transactions paid 5,000 lamports each. Priority fees
are zero, and all context-account rent is returned during cleanup.

The live recurring-liquidity path rolled a trader from auction 528 into
auction 819 in one 4,093-byte transaction. It privately credited a payout of
9 base units and 7,155 quote units, then placed the next order. The successful
transaction is
`3C1j93M4Hc85T5NpzmfJbCCBcyL9wuJ5AgRoYrqH4B8iBcQ8VV5iNCBxme1MAjPLLW2LmBMCLkDu92CVQq49Ck2c`.
The path was then repeated from a freshly submitted, folded, and settled
auction 822 into auction 823. That second 4,093-byte rollover consumed 694,958
CU, paid the 5,000-lamport base fee with zero priority fee, and finalized as
`4UGNJtAXKwEHFFUKTPtw29LeZrjU73BGfzLinKhPXHArPWY1KySYUBhscWgq9XNXtTkTLxe1tXnsgsh1Ym8sLJfC`.
The trader then replaced that auction 823 order in one 3,618-byte transaction;
the two initial/replacement queue deltas folded together and the auction
settled normally. This verifies that order edits do not require a withdrawal
or a new auction.

Build the SBF v0 test artifact and run the ignored LiteSVM suite with:

```sh
cargo-build-sbf --arch v0 --features sbf-test-fixtures \
  --manifest-path programs/beatex/Cargo.toml \
  --sbf-out-dir target/deploy-v0

BEATEX_SBF_SO="$PWD/target/deploy-v0/beatex.so" \
  cargo test -p beatex-program --features sbf-test-fixtures \
  --test sbf_groth16 -- --ignored --nocapture
```

LiteSVM 0.10 embeds Token-2022 v10 without the `zk-ops` feature; confidential
instructions on that bundled binary return `InvalidInstructionData`. Download
the official v11.0.0 SBF artifact and override the embedded program for the
confidential-vault CPI tests:

```sh
curl -L --fail \
  https://github.com/solana-program/token-2022/releases/download/program%40v11.0.0/spl_token_2022-11.0.0.so \
  -o target/deploy-v0/spl_token_2022-11.0.0.so

BEATEX_SBF_SO="$PWD/target/deploy-v0/beatex.so" \
TOKEN_2022_SBF_SO="$PWD/target/deploy-v0/spl_token_2022-11.0.0.so" \
  cargo test -p beatex-program --features sbf-test-fixtures \
  --test sbf_token2022 -- --ignored --nocapture
```

The vault tests exercise PDA signing, the optional Token-2022 confidential
bridge, and the default public-custody path. The default proved deposit performs
two public `TransferChecked` CPIs and commits the same amounts to the trader's
encrypted balance; the proved withdrawal performs the reverse encrypted
transition and two signed public transfers. Current SBF measurements are
132,113 CU for deposit, 132,157 CU for withdrawal, and 121,209 CU for an
internal cancellation refund.

Run the expensive exact production-claim fixture separately in release mode:

```sh
BEATEX_SBF_SO="$PWD/target/deploy-v0/beatex.so" \
TOKEN_2022_SBF_SO="$PWD/target/deploy-v0/spl_token_2022-11.0.0.so" \
  cargo test --release -p beatex-vault-proof-service \
  --test full_withdrawal_sbf -- --ignored --nocapture
```

It generates the real 2.9-million-constraint proof, creates both standard
Token-2022 proof bundles, and executes the production atomic claim handler.

The SBF suite performs encrypted order chunking and aggregation, verifies ten
3-of-7 threshold decryptions, publishes the aggregate curves, clears the
auction, and exercises permissionless timeout cancellation. Current LiteSVM
measurements are 40,093 CU for the stateless 3-of-7 verifier, 80,298 CU for a
one-public-input Groth16 proof, 266,074 CU for the single aggregate-publication
transaction, and 9,785 CU for clearing a five-level grid. Committee shares are
private proof witnesses, so no preceding share-submission transactions or PDAs
are required.

This code is prototype cryptography and has not been audited.

## Browser proving checkpoint

SP1 v6.8.1's complete prover does not compile for browser WebAssembly. The
repository therefore includes an entirely-WASM direct Arkworks Groth16
benchmark as the closest viable client-side path. On an Apple M4 in Chrome 152,
the arithmetic-only circuit produced proofs in a median 28.4 ms for 3 levels and
131.5 ms for 17 levels. The subsequently implemented exact binding costs
5,176,032 constraints for one ciphertext and 175,564,998 constraints for the
current 17-level, two-ciphertext-per-level order. It compiles to WASM, but its
proving key would be tens of GiB at the full size, so direct browser Groth16 is
not viable for this relation. See
[`crates/browser-prover-bench/RESULTS.md`](./crates/browser-prover-bench/RESULTS.md).

The native-proof composition is fully browser-WASM capable. For a
17-level order, Chrome generated 17 batched grouped-ciphertext-validity proofs
and five U64 range proofs in 87.3 ms median. At 65 levels it took 334.8 ms. The
17-level case needs about 14 packed proof-verification transactions, so it does
not meet the single-order-transaction UX requirement. See
[`crates/zk-elgamal-bench/RESULTS.md`](./crates/zk-elgamal-bench/RESULTS.md).

The current independent queued-order circuit implements the real 17-level
order relation plus encrypted balance deductions in 241,198 constraints and
proves in a 2.198-second native median. Its approximately 3,032-byte complete
transaction estimate fits Solana's 4,096-byte v1 limit. Direct
onchain aggregation is nevertheless rejected: 68 additions exceed the 1.4M CU
cap even before proof verification, and compressed-point recovery is much more
expensive. The implemented architecture stores independent accepted orders and
lets a permissionless bot fold up to four orders into the aggregate with one
518,962-constraint Groth16 transition proof. Accepted/folded counter equality
gates decryption after collection, and an incomplete queue can be cancelled at
the aggregation deadline. See
[`crates/babyjubjub-groth16-spike/RESULTS.md`](./crates/babyjubjub-groth16-spike/RESULTS.md)
and
[`crates/babyjub-sbf-spike/README.md`](./crates/babyjub-sbf-spike/README.md).

The production-shaped trader prover now compiles the actual four circuit
families into a 1.6 MiB `wasm-bindgen` runtime. A real single-threaded Node WASM
integration generated valid proofs in 1.87 seconds for deposit, 1.95 seconds
for public withdrawal, 15.21 seconds for a 17-level order, 2.32 seconds for
collateral release, and 2.63 seconds for
internal settlement; composed replacement and rollover took 16.00 and 16.29
seconds. Content-addressed canonical-uncompressed proving keys load in roughly
22 ms for balance/release/settlement and 174 ms for orders after their SHA-256
integrity checks; their respective asset sizes are 9.8 MiB, 83.8 MiB, 10.4 MiB,
and 11.3 MiB. These are WASM integration measurements, not a browser latency
claim; Chrome memory and latency still need measurement in the final frontend.

The Anchor program stores circuit-specific Groth16 verification keys in
authority-initialized PDAs seeded by market, proof purpose, and circuit version.
It reconstructs each Poseidon public input onchain before verification; callers
cannot supply an unrelated public input or substitute a key from another proof
purpose.
