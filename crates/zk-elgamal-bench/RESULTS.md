# Native ZK ElGamal benchmark results

Measured on an Apple M4 in Chrome 152 and with the Rust release build. Each
price level contains separate four-bit buy and sell quantities, encrypted as
two-handle grouped ciphertexts. Times are medians of five rounds.

| Levels | Quantities | Validity proofs | Range proofs | Packed proof transactions | Proof data | Native prove | Browser-WASM prove |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 3 | 6 | 3 | 1 | 3 | 2,184 B | 10.7 ms | 17.8 ms |
| 9 | 18 | 9 | 3 | 8 | 6,552 B | 24.3 ms | 52.4 ms |
| 17 | 34 | 17 | 5 | 14 | 11,752 B | 41.2 ms | 87.3 ms |
| 33 | 66 | 33 | 10 | 27 | 23,088 B | 82.7 ms | 174.5 ms |
| 65 | 130 | 65 | 19 | 52 | 44,824 B | 158.4 ms | 334.8 ms |

“Prove” is validity-proof time plus range-proof time; ciphertext encryption is
reported separately by the executable. The packed transaction count assumes at
most two 417-byte validity-proof instructions per transaction and one 937-byte
range-proof instruction per transaction. It excludes order writes and context
account creation/closure.

The Agave 3.1.10 compute schedule assigns approximately 13,000 CU to a batched
two-handle validity proof and 111,000 CU to a U64 range proof. This implies the
following aggregate verification work, distributed across the proof
transactions: 150k, 450k, 776k, 1.539m, and 2.954m CU respectively. These are
schedule-derived estimates, not Devnet measurements; cluster software and SDK
transcript compatibility must be checked on Devnet.

## Interpretation

- Browser proof generation is fast enough even at 65 levels.
- The native proof program supports the 17-level Devnet configuration comfortably, but requires
  14 proof-verification transactions before order-write/finalize transactions.
- 33 and 65 levels remain cryptographically feasible, but require 27 and 52
  proof transactions and a sharded replacement for the current monolithic
  auction account. They are poor Devnet defaults.
- The recommended current cap remains 17 levels. The cap is caused by the
  account layout and transaction UX, not browser proving time.

## Security boundary

The measured composition proves that every buy and sell value is in `0..=15`
and that each ciphertext is a well-formed encryption under the committee and
recovery keys. It does **not** privately prove that at least one of buy or sell
is zero at each level. BeatEx still needs a side-hiding Ristretto OR proof (or an
equivalent protocol change) before untrusted order finalization is sound.

An offset-signed single value cannot replace the two ciphertexts: homomorphic
aggregation would recover only net flow at a level and would destroy the gross
demand and supply curves required by clearing.
