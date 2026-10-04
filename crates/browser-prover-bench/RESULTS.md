# Browser-local proving results

Measured on 2026-10-02 in Chrome 152 on an Apple M4 MacBook Air with 16 GB
RAM. Every timed operation below ran in single-threaded
`wasm32-unknown-unknown`; there was no native helper, worker, GPU, or remote
prover.

Each configuration used a fresh browser tab and ten independent proofs. The
release WASM payload produced by `wasm-bindgen` was 872 KiB.

| Price levels | Constraints | Setup wall time | Median proof | Mean proof | Proof range | Median verify | WASM memory after proofs | Proving key (compressed) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 185 | 57.0 ms | 28.4 ms | 28.48 ms | 28.0–30.0 ms | 2.5 ms | 1.69 MiB | 36.9 KiB |
| 17 | 1,039 | 256.7 ms | 131.5 ms | 130.78 ms | 120.8–137.2 ms | 2.8 ms | 3.94 MiB | 224.1 KiB |

Every proof verified successfully and was exported to the 256-byte
representation expected by the Solana Groth16 verifier. Export took no more
than 0.01 ms on average. Setup is shown separately and is not part of per-order
proof latency.

These timing measurements are a lower bound, not a complete BeatEx order proof.
The timed circuit enforces only bounded quantities, per-price buy/sell
exclusivity, and public buy/sell totals.

## Exact Ristretto ciphertext-binding result

The lower bound has now been replaced by a separately implemented exact
ciphertext relation. The circuit uses Arkworks R1CS over BN254 and
constraint-optimized `EmulatedFpVar` arithmetic for the Ristretto255 base field.
It enforces canonical compressed encodings, Ristretto decompression sign rules,
extended-Edwards arithmetic, Ristretto quotient equality, and both Solana
twisted-ElGamal equations:

```text
C = q*G + r*H
D = r*P
```

Release-mode synthesis produced:

| Shape | Ciphertext bindings | Constraints | Public inputs | Witness variables |
| --- | ---: | ---: | ---: | ---: |
| Shared committee-key decode | 0 | 12,730 | 2 | 11,780 |
| One exact ciphertext | 1 | 5,176,032 | 6 | 5,150,283 |
| 3 levels, buy and sell ciphertexts | 6 | 30,992,542 | 26 | 30,842,798 |
| 17 levels, buy and sell ciphertexts | 34 | 175,564,998 | 138 | 174,720,882 |

The Arkworks counters include the implicit constant-one instance variable, so
the table's public-input column subtracts one. A ciphertext mutation test makes
the circuit unsatisfied. The same source builds successfully for
`wasm32-unknown-unknown`.

The shape is exactly linear after the committee public key is decoded once:
every ciphertext adds 5,163,302 constraints, four packed public inputs, and
5,138,503 witness variables. Small order-level checks are negligible next to
that cost. This is not merely a slow browser benchmark: the 17-level circuit
would require a proving key on the order of tens of GiB. Generating a Groth16
proof for this direct design in a browser is therefore not operationally
credible.

## SP1 browser-prover result

SP1 v6.8.1 cannot currently produce a proof entirely in browser WebAssembly,
so there is no honest SP1 browser proof-time number to report. A real
`wasm32-unknown-unknown` compile of `sp1-sdk`'s local CPU prover failed first on
browser randomness. Enabling the required JavaScript randomness feature
advanced the build to its next hard failure: SP1's native Tokio networking and
process stack enables `mio`, which explicitly rejects this WASM target.

The remaining shipped prover path also relies on native memory mapping/JIT
components and a terminal Groth16 wrapper implemented through Go/cgo or Docker.
Those are architectural porting tasks, not missing build flags. The standalone
SP1 verifier does support WASM, but verification is not proof generation.

## Interpretation

Browser-local direct Groth16 is fast for small native BN254 arithmetic, but the
exact cross-curve relation rules it out for the order-submission path. The next
prototype should use the native ZK ElGamal Proof Program for Ristretto
ciphertext validity and range proofs while retaining separate buy and sell
ciphertexts. A single signed/offset quantity would preserve only net flow after
aggregation and would lose the gross demand and supply curves. Private
buy/sell exclusivity therefore needs a compact side-hiding Ristretto OR proof.
Groth16 can remain for relations whose arithmetic is native to BN254 and that
do not reproduce every Ristretto scalar multiplication. The replacement proof
composition and measurements are in `../zk-elgamal-bench/RESULTS.md`.
