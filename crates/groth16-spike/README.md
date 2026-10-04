# BeatEx Groth16 feasibility spike

This standalone, host-only crate demonstrates deterministic BN254 Groth16
setup, proving, Arkworks verification, and verification of exported bytes with
Light Protocol's `groth16-solana` verifier.

The small test circuit has three price levels. At each level it proves that:

- buy and sell quantities are not both non-zero; and
- each quantity is an integer in the inclusive range `0..=15`.

The public inputs are the sum of all private buy quantities followed by the sum
of all private sell quantities. They make input ordering and byte conversion
testable, but they are **not** a complete production order binding.

## Important limitation

This spike does **not** bind private BN254 witnesses to BeatEx's Ristretto
twisted-ElGamal ciphertexts. A proof produced by this crate only proves that
some bounded, side-exclusive quantities exist with the public totals. It must
not be accepted as an order-validity proof in the Solana program. Production
use still requires a reviewed Ristretto ciphertext-binding construction and
domain-separated market/auction/order public inputs.

## Run

```sh
cargo test --manifest-path crates/groth16-spike/Cargo.toml --locked
```

The exported convention matches `groth16-solana` 0.2.0:

- all field coordinates and public inputs are fixed-width big-endian;
- G1 is `x || y` (64 bytes);
- G2 is `x.c1 || x.c0 || y.c1 || y.c0` (128 bytes);
- proof A is negated for the verifier's four-pair equation;
- proof B and C are not negated; and
- the verifying key stores alpha G1, beta/gamma/delta G2, and each IC G1 in
  the same uncompressed syscall layout.
