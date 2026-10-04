# BN254 G1 ElGamal circuit spike

This isolated rejection spike tests whether standard ElGamal on BN254 G1 can
combine cheap Solana `alt_bn128` syscalls with a practical Groth16
ciphertext-binding circuit.

The circuit implements the actual one-ciphertext equations

```text
C1 = rG
C2 = mG + rY
```

for a private 254-bit scalar `r`, a four-bit private message `m`, and a
variable committee key `Y`. BN254 G1 coordinates are in `Fq`, while Groth16's
constraint field is `Fr`; the implementation therefore uses Arkworks'
emulated-field gadget and complete Renes-Costello-Batina projective addition.

Release-mode synthesis measured:

| Relation | R1CS constraints |
| --- | ---: |
| One G1 ElGamal ciphertext | 7,229,904 |
| 34 ciphertexts, linear extrapolation before order logic | 245,816,736 |

This is worse than the measured Ristretto relation and rules out a complete
17-level Groth16 order circuit despite the low onchain G1 addition cost. The
program prints the exact synthesized one-ciphertext shape:

```sh
cargo run --release --manifest-path crates/bn254-g1-groth16-spike/Cargo.toml
```

The extrapolation is not used as a security result; the real
one-ciphertext relation is sufficient to reject this architecture by several
orders of magnitude. This spike is not production cryptography.
