# Baby Jubjub circuit result

Measured on the development machine in release mode on 2026-10-04. Times are
three-round medians for proving and verification and 21-round medians for
encryption preprocessing.

## Circuit shape

| Component | R1CS constraints |
| --- | ---: |
| 17-level order and exact collateral maxima | 1,879 |
| 36 Baby Jubjub ElGamal encryptions | 163,429 |
| Chained width-13 Poseidon transcript | 7,522 |
| **Total** | **172,830** |

The 36 ciphertexts are 34 independent order ciphertexts plus encrypted
`R_A` and `R_B`. The circuit has one Groth16 public input (the transcript
digest), two instance variables including the constant-one variable, and
172,746 witness variables.

## Host timings and sizes

| Measurement | Result |
| --- | ---: |
| Encrypt 36 values | 3.170 ms |
| Encrypt and compute transcript | 6.140 ms |
| Circuit-specific setup | 1,772.072 ms |
| Groth16 proving | 1,955.966 ms |
| Groth16 host verification | 0.721 ms |
| Compressed proving key | 36,028,592 bytes |
| Compressed Arkworks proof | 128 bytes |
| Solana uncompressed Groth16 proof | 256 bytes |
| 72 compressed Baby Jubjub points | 2,304 bytes |
| Ciphertexts + Solana proof + digest | 2,592 bytes |

The raw payload size excludes signatures, account keys, blockhash,
instructions and other transaction framing.

## What this proves

- Every order quantity is four-bit and therefore in `0..=15`.
- Buy and sell cannot both be nonzero at one price level.
- Prices follow the 17-level public reference/tick grid without field wrap.
- `R_A` and `R_B` are the exact maxima required by the contingent order.
- Every order and reserve value is correctly encrypted under the same
  prime-order committee key with independent randomness.
- The public digest binds the proof to the ciphertexts and auction context.

## What remains

- Chrome proving time and peak memory have not been measured for this circuit.
  The production wrapper has been exercised in single-threaded Node WASM; its
  final 234k-constraint queued-order result is recorded below.
- The direct onchain path was benchmarked separately in
  `crates/babyjub-sbf-spike`. Sixty-eight additions alone exceed the 1.4M CU
  transaction cap, and syscall-assisted compressed-point recovery costs about
  499k CU for one representative point. Direct Baby Jubjub aggregation is
  therefore rejected for the one-transaction order path.
- Real-browser time and peak memory for the final 234k-constraint queued-order
  circuit remain to be measured.
- Compression and subgroup-validation rules must become protocol constants;
  they must not depend implicitly on a future Arkworks serialization change.
- This is a feasibility spike, not an audited production circuit or trusted
  setup.

## Queue submission and permissionless fold

The implemented architecture removes aggregate state from the trader's proof.
One order proof validates an independent queued order and the trader's two
private balance deductions:

```text
new_base_balance  + R_A = old_base_balance
new_quote_balance + R_B = old_quote_balance
```

All four old/new balance ciphertexts are opened in the circuit. The balance
ciphertexts, order ciphertexts, auction, owner, nonce, price grid, committee
key and salted private-order commitment are bound by one chained Poseidon
digest. The order transaction increments only an accepted-order counter; it
does not depend on the aggregate revision.

A permissionless bot later proves a batch of up to four transitions from the
old encrypted aggregate to the new encrypted aggregate. The fold proof opens
the queued ciphertexts and proves all 68 Baby Jubjub additions per order
offchain. Onchain, the program hashes the old/new aggregate encodings and the
stored per-order ciphertext digests, verifies one Groth16 proof, marks each
order `Folded`, and advances the folded counter. Folding is allowed while the
auction is still collecting orders.

| Measurement | Result |
| --- | ---: |
| Queued-order constraints | 234,166 |
| Queued-order setup | 2,164.339 ms |
| Queued-order proving median | 2,195.519 ms |
| Queued-order host verification median | 0.710 ms |
| Queued-order compressed proving key | 43,932,592 bytes |
| Four-order fold constraints | 310,870 |
| Four-order fold setup | 4,386.628 ms |
| Four-order fold proving median | 3,842.399 ms |
| Four-order fold host verification median | 0.709 ms |
| Four-order fold compressed proving key | 56,365,360 bytes |
| Raw queued-order payload | 2,624 bytes |
| Estimated complete queued-order Solana v1 transaction | about 3,032 bytes |
| Raw fold payload | 2,464 bytes |

The queue/fold test suite rejects insufficient balances, buy/sell overlap,
mutated order ciphertexts, mutated balance ciphertexts, and mutated fold-order
ciphertexts. Inactive fold slots are constrained to canonical identity
ciphertexts and zero context.

## Aggregate decryption and publication

The publication circuit accepts any sorted 3-of-7 committee subset. It binds
the three registered Baby Jubjub public shares, selects the corresponding
Lagrange coefficients inside the circuit, and verifies three batched
Chaum-Pedersen relations over all 34 aggregate ciphertexts. The same
transcript-derived coefficients then check all published demand/supply values
with one batched threshold-decryption equation. Committee decryption shares
and DLEQ transcripts are private witnesses; all 34 aggregate quantities remain
public.

| Measurement | Result |
| --- | ---: |
| Constraints | 518,852 |
| Circuit setup | 5,753.211 ms |
| Native proving median | 6,010.253 ms |
| Native verification median | 1.258 ms |
| Compressed proving key | 94,048,400 bytes |
| Compact publication instruction data | about 560 bytes |
| SBF publication instruction | 268,990 CU |
| Complete publication stage | one transaction |

The Groth16 public digest is reconstructed by the program from the auction,
accepted-order count, committee key, selected participant IDs, their three
registered public shares, the frozen 34 ciphertexts, and the 34 published
quantities. A committee coordinator supplies the private shares and DLEQ data
to the prover. Anyone may relay the resulting proof and public curves in the
single permissionless publication transaction.

## Public deposit to encrypted balance

The balance-deposit circuit proves that the new encrypted base and quote
balances are the old balances plus the exact public Token-2022 deposit amounts
under the registered Baby Jubjub committee key. It binds the market, owner,
key epoch, revision, amounts, key, and all four old/new ciphertexts.

| Measurement | Result |
| --- | ---: |
| Constraints | 26,885 |
| Circuit setup | 350.524 ms |
| Native proving median | 380.048 ms |
| Compressed proving key | 5,127,920 bytes |
| Estimated instruction payload | about 432 bytes |
| Full two-asset SBF deposit path | 132,113 CU |

The SBF path verifies the proof, moves public tokens from the trader sources
to the market vaults, verifies exact source and vault deltas, and only then
advances the encrypted private-balance revision. The funds remain in the
vaults' public Token-2022 balances while trading state remains encrypted.

The public withdrawal uses the same relation in reverse: the remaining private
balance plus the declared withdrawal must equal the current balance. Its
release test, including deterministic setup and proof generation, completes in
under 0.7 seconds on an Apple M4. The two-asset SBF withdrawal consumes 132,157
CU and transfers both public amounts in one transaction.

## Optional confidential Token-2022 claim and cancellation refund

The withdrawal circuit reopens the salted order commitment authenticated at
submission, selects the buy/sell quantities at only the clearing index, and
enforces exact-index pro-rata allocation. The long side is rounded adversely:
base received and quote proceeds round down, while base spent and quote paid
round up. A separate refund circuit returns the two original reserves.

Both circuits bind the full standard Token-2022 transfer-proof contexts and
combine the two private 48-bit payout commitments with a transcript-derived
128-bit coefficient. One exact emulated-Ristretto Pedersen equation inside
BN254 connects the private payouts to those commitments. The program performs
the same commitment combination natively before verifying the one-input
Groth16 proof.

| Measurement | Claim | Refund |
| --- | ---: | ---: |
| Constraints | 2,924,068 | 2,923,244 |
| Instance variables | 2 | 2 |
| Witness variables | 2,908,453 | 2,907,651 |
| Exact circuit satisfaction, release | 17.96 s | 18.19 s |
| Deterministic setup, Apple M4 | 43.838 s | not measured |
| Native proving, Apple M4 | 40.833 s | not measured |
| Native host verification | 1.331 ms | not measured |
| Full production SBF claim + two Token-2022 CPIs | 265,954 CU | not measured |

The proof is correct but is not presently suitable for browser generation or
an interactive claim UX. The prototype therefore assigns claim/refund proving
to the committee-side vault-proof service. Faster proving, GPU acceleration,
or a different exact cross-curve backend remains Devnet-integration work.

The full SBF fixture uses independently generated standard Token-2022 proof
bundles, the exact program-bound claim witness, and the production withdrawal
handler. Both confidential destination pending balances and the terminal order
state are checked after the atomic transaction.

## Encrypted-balance rollover and order modification

Recurring market makers can settle directly into their encrypted balance and
place the next order without the Ristretto/Token-2022 bridge. The rollover and
order proofs are independent once the intermediate ciphertexts are selected and
can therefore be generated concurrently.

| Measurement | Rollover settlement | Collateral release |
| --- | ---: | ---: |
| Constraints | 31,880 | 29,024 |
| Recurring proof, Apple M4 | 0.526–0.549 s | 0.425–0.429 s |

The ordinary 234,166-constraint order proof remains the latency-dominant part
of rollover at roughly 2.2 seconds. Replacement composes the smaller collateral
release proof with that same order proof; cancellation needs only the release
proof.

## Trader WebAssembly integration

`crates/trader-wasm-prover` wraps the four trader circuit families in one
single-threaded `wasm-bindgen` runtime. `packages/prover-wasm` loads that runtime
in a worker and fetches content-addressed proving keys on demand. An actual Node
WebAssembly integration on the development Apple M4 measured:

| Operation | WASM proving time |
| --- | ---: |
| Deposit | 1.87 s |
| Public withdrawal | 1.95 s |
| Queued order | 15.21 s |
| Collateral release | 2.32 s |
| Internal settlement | 2.63 s |
| Replacement | 16.00 s |
| Rollover | 16.29 s |

The generated runtime is 1,715,928 bytes. Canonical-uncompressed Devnet keys are
10,255,792 bytes for balance transitions, 87,865,136 bytes for orders,
10,941,872 bytes for release, and 11,847,792 bytes for settlement. Key decoding
is a separate one-time session cost and is intentionally lazy. After SHA-256
asset authentication, unchecked decoding took 21.6 ms for balance, 173.9 ms for
order, 21.8 ms for release, and 22.4 ms for settlement. This replaces the
canonical-compressed format, whose order key was half the size but took 52.87
seconds to decode. These Node WASM results prove the production boundary works,
but they do not replace Chrome latency and peak-memory measurements.
