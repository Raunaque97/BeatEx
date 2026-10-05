# BeatEx

Confidential frequent batch auctions on Solana.

The repository now contains the Anchor program, Baby Jubjub order/fold,
deposit, aggregate-publication, private-claim, and cancellation-refund Groth16 circuits, Token-2022 confidential
vault integration, LiteSVM/SBF benchmarks, and local demo-token/deployment
tooling described in [`TECHNICAL_DESIGN.md`](./TECHNICAL_DESIGN.md).

## Auction FAQ

### Can one wallet submit more than one order in an auction?

The current program accepts multiple immutable orders: every submission gets a
new sequence-based PDA, and submitting again does not cancel the earlier order.
There is no order-cancel or order-replace instruction yet.

The intended replacement design will keep one private `ActiveOrder` per
`(auction, owner)` without rejecting later submissions. A replace proof will
release the previous private reserve, lock the replacement reserve, and enqueue
an encrypted delta `new_order - old_order`. A cancel proof will enqueue
`-old_order` and return its reserve to the trader's encrypted balance. Folding
all accepted delta events therefore leaves exactly the latest order in the
aggregate while still allowing bots to fold during collection. The final active
revision, rather than every historical event, will be claimable. Observers can
see that an authority submitted, changed, or cancelled an order, but not its
private quantities.

This preserves ordinary cancel/replace UX. It also prevents one signing
authority from leaving simultaneous opposite orders in the auction, although a
permissionless market cannot prevent the same person from using unrelated
wallets.

### Can a market maker claim auction N and place auction N+1's order atomically?

Not by merely packing the existing claim and submission instructions together.
The claim pays confidential Token-2022 accounts, while submission spends the
trader's separate Baby Jubjub encrypted balance; the combined payload would
also be too close to or beyond the transaction-size budget.

The intended optimization is a dedicated `rollover_order` path. One proof
would settle the old private order into the trader's encrypted balance, deduct
the next order's reserves, create the new queued order, and terminally mark the
old order, without withdrawing and redepositing tokens. It should require one
Groth16 verification and no Token-2022 transfer proof contexts. The prototype
should initially permit rollover only when both auctions use the same key
epoch; key-rotation cases can use the existing claim/deposit path.

### How is an auction's reference price selected?

The first-version policy uses the previous clearing price when the previous
clearing volume meets a configured minimum; otherwise it uses a fresh Pyth
BTC/USD price. The first auction has previous volume zero and therefore uses
Pyth. An immutable exponent adjustment converts the feed into quote lots per
base lot (for 8-decimal tBTC / 6-decimal tUSD it is `-2`), then the price is
rounded to the nearest tick before the 17-level grid is constructed. A stale update, wrong feed,
excessive confidence interval, or arithmetic overflow makes initialization fail
closed. The auction records the chosen reference price and its source.

Pyth already runs its receiver on Solana Devnet, and its sponsored shard-zero
BTC/USD feed is available there. BeatEx therefore does not need to deploy a demo
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
- **Phase 4 — Confidential collateral (proved public deposits and Token-2022 vault linkage complete)**
- **Phase 5 — Private claims and cancellation fallback (complete in host and SBF tests)**
- **Phase 6 — Devnet demo hardening (program and named confidential demo assets deployed; scripted full-auction run pending)**

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
- native two-handle grouped-ciphertext validity and strict four-bit range-proof
  benchmarks in Rust and browser WebAssembly;
- a clean default-stack SBF build with a measured monolithic grid cap of 17
  price levels (19 is the first failing account layout).

Run the checks with:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Run the current encrypted multi-trader simulation with:

```sh
cargo run -p beatex-demo-sim
```

It encrypts three private order curves, homomorphically aggregates every grid
entry, verifies participant-specific 3-of-7 decryption-share proofs, publishes
only the aggregate curves, and applies the deterministic clearing rule.

The current exact private-claim prover is committee-side, not browser-side. On
an Apple M4 its Arkworks release benchmark takes 40.833 seconds per proof after
the one-time setup; host verification is 1.331 ms. The circuit is deliberately
kept exact while faster proving is treated as demo-hardening work.

The hackathon environment uses named Token-2022 test assets `tBTC` and `tUSD`.
The idempotent local-validator/Devnet mint workflow and current toolchain
requirements are documented in
[`docs/DEMO_ENVIRONMENT.md`](./docs/DEMO_ENVIRONMENT.md).

The current SBF program is deployed on Devnet at
`2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd`; the retained Devnet tBTC and
tUSD addresses are recorded in `docs/DEMO_ENVIRONMENT.md`.

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

The vault tests exercise PDA signing, Token-2022 `ApplyPendingBalance`, two
confidential `Transfer` CPIs, and the full proved-deposit path. The latter
performs two public `TransferChecked` CPIs followed by two confidential
`Deposit` CPIs, then commits the same amounts to the trader's encrypted
balance. Measured costs are 15,601 CU for apply-pending, 51,258 CU for two
transfers, and 157,151 CU for the complete two-asset deposit instruction.

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
order relation plus encrypted balance deductions in 234,166 constraints and
proves in a 2.196-second native median. Its approximately 3,032-byte complete
transaction estimate fits Solana's 4,096-byte v1 limit. Direct
onchain aggregation is nevertheless rejected: 68 additions exceed the 1.4M CU
cap even before proof verification, and compressed-point recovery is much more
expensive. The implemented architecture stores independent accepted orders and
lets a permissionless bot fold up to four orders into the aggregate with one
310,870-constraint Groth16 transition proof. Accepted/folded counter equality
gates decryption after collection, and an incomplete queue can be cancelled at
the aggregation deadline. See
[`crates/babyjubjub-groth16-spike/RESULTS.md`](./crates/babyjubjub-groth16-spike/RESULTS.md)
and
[`crates/babyjub-sbf-spike/README.md`](./crates/babyjub-sbf-spike/README.md).

The Anchor program stores circuit-specific Groth16 verification keys in
authority-initialized PDAs seeded by market, proof purpose, and circuit version.
It reconstructs each Poseidon public input onchain before verification; callers
cannot supply an unrelated public input or substitute a key from another proof
purpose.
