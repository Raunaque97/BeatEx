# BeatEx Baby Jubjub Groth16 spike

This isolated host benchmark implements BeatEx's 17-level contingent-order
relation over Baby Jubjub, whose base field is the BN254 Groth16 scalar field.
It is intentionally outside the repository workspace while the architecture is
being reviewed.

For every price level `i`:

```text
buy[i], sell[i] are amounts of base asset A in 0..=15 lots
P[i] is quote asset B lots per base asset A lot
buy[i] * sell[i] = 0
```

Only the selected clearing level executes. The circuit consequently proves the
exact collateral maxima, not sums across levels:

```text
R_A = max_i(sell[i])
R_B = max_i(P[i] * buy[i])
```

The circuit encrypts all 34 order quantities and both collateral maxima with
exponential ElGamal:

```text
Enc_Y(m; r) = (rG, mG + rY)
```

`G` is the prime-order Baby Jubjub generator and `Y` is the committee public
key. Independent randomness is required for every ciphertext.

## Public binding

The Groth16 proof has one public input: a chained Poseidon transcript digest.
The transcript commits to the protocol domain/version, the auction and owner
public keys encoded injectively as two 128-bit limbs each, the client order
nonce, committee key, price-grid parameters, and all ciphertext point
coordinates.
It uses the same BN254 x^5 width-13 parameters as `light-poseidon` / Solana's
Poseidon syscall. Each frame hashes:

```text
[previous_digest, up to 11 transcript field elements, zero padding]
```

The Solana program must recompute this digest from its trusted auction state
and the submitted ciphertexts before Groth16 verification. The digest approach
avoids 144 ciphertext-coordinate public inputs.

The wire-size measurement uses Arkworks' 32-byte compressed twisted-Edwards
encoding: canonical little-endian `y` with the sign of `x` in the high flag
bit. The onchain decoder must reject non-canonical coordinates, invalid curve
points, and points outside the prime-order subgroup.

## Run

```sh
cargo test --release --manifest-path crates/babyjubjub-groth16-spike/Cargo.toml
cargo run --release --manifest-path crates/babyjubjub-groth16-spike/Cargo.toml
```

See [RESULTS.md](RESULTS.md) for the recorded host result and limitations.

## Sharded transition circuit

`src/transition.rs` implements the full order-submission transition proposed
after direct Baby Jubjub aggregation exceeded Solana's CU cap. It proves the
private order arithmetic and encryption, adds the order ciphertexts to one old
aggregate shard inside Groth16, and proves exact `R_A` / `R_B` deductions from
the trader's encrypted 64-bit base and quote balances. The Solana program only
hashes compressed old/new state and verifies Groth16; it performs no Baby
Jubjub group operation.
