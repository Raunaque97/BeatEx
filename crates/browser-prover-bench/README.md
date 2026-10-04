# BeatEx browser-local Groth16 benchmark

This isolated crate contains two Arkworks BN254 Groth16 experiments:

1. an entirely-WebAssembly timing benchmark for the small order-arithmetic
   relation; and
2. an exact Solana-compatible Ristretto255 twisted-ElGamal binding circuit used
   to measure the real cross-curve constraint cost.

It copies the existing `crates/groth16-spike` arithmetic relation at two sizes:

- 3 price levels, matching the existing feasibility spike;
- 17 price levels, a more useful small order grid.

At each level, private buy and sell quantities are in `0..=15` and cannot both
be non-zero. Public inputs are total buy and total sell quantity. The proving
key is generated deterministically in WASM, measured separately, and retained
for the timed per-proof path.

## Exact Ristretto relation

`src/ristretto.rs` constrains the canonical public encodings and proves knowledge
of quantity `q` and opening `r` such that Solana's ciphertext is exactly:

```text
C = q*G + r*H
D = r*P
```

It uses Solana's `G`, derives `H` with the same SHA3-512 construction, performs
Ristretto decompression and Edwards group arithmetic over the emulated
`2^255-19` field, and applies Ristretto quotient-group equality. The public key,
commitment, and handle are each packed into two 128-bit BN254 public inputs.

The resulting circuit is correct but not viable: a single binding requires
5,176,032 constraints, including 12,730 constraints shared by the public-key
decode. Each additional ciphertext adds 5,163,302 constraints. The detailed
results are in [`RESULTS.md`](./RESULTS.md).

The timed 3- and 17-level proof results below still concern only the arithmetic
control circuit. They are retained to distinguish ordinary Groth16 cost from
the cross-curve cost and are not production BeatEx order proofs.

## Build and run

Install `wasm-pack`, then from this directory:

```sh
wasm-pack build --target web --release --out-dir web/pkg
python3 -m http.server 4173 --directory web
```

Open <http://localhost:4173>. Run one level count per fresh page load so the
WebAssembly memory baseline is meaningful. For automated runs, query parameters
are supported, for example:

```text
http://localhost:4173/?levels=17&rounds=5&autorun=1
```

The JSON report includes:

- deterministic trusted-setup time (not per-order latency);
- per-round proof, 256-byte Solana proof export, and Arkworks verify time;
- constraint, instance-variable, and witness-variable counts;
- compressed and uncompressed proving-key sizes;
- committed WASM linear-memory bytes before setup, after setup, and after proofs;
- browser user agent and cross-origin-isolation status.

Committed WASM memory is reported because it is portable across browsers. It is
not live heap or process RSS, grows in 64 KiB pages, and does not shrink.

The first measured results are recorded in [`RESULTS.md`](./RESULTS.md).

The WASM export `ristrettoConstraintReport(bindings)` synthesizes the real
zero- and one-binding circuits and returns the exact linear shape for the
requested number of ciphertext bindings. It intentionally does not attempt a
Groth16 setup: one binding already implies an approximately 1 GiB proving key,
and a 17-level two-sided order would be tens of GiB.
