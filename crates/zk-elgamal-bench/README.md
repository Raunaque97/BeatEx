# BeatEx native ZK ElGamal benchmark

This harness measures the proof composition available from Solana's native ZK
ElGamal stack for the existing two-sided order representation.

For every price level it creates a buy and sell quantity in `0..=15`, encrypts
both as two-handle grouped ciphertexts, and generates:

- one batched grouped-ciphertext-validity proof for the two ciphertexts; and
- strict four-bit batched range proofs, with at most seven real quantities plus
  one random zero-valued padding commitment per 64-bit proof.

Run the release benchmark with:

```sh
cargo run --release -p beatex-zk-elgamal-bench -- --rounds 5
```

For the actual browser-WASM package:

```sh
npm install
python3 -m http.server 4174
```

Then open `http://localhost:4174/web/?autorun=1`. The page uses the official
`@solana/zk-sdk` web build rather than a native helper.

The benchmark reports client proof-generation time, host verification time,
proof/context byte counts, and the number of proof-verification transactions.
Host verification time is not Solana compute-unit usage.

Measured results and the practical grid-size conclusion are in
[`RESULTS.md`](./RESULTS.md).

These standard proofs do not privately enforce `buy * sell = 0`. BeatEx still
needs a side-hiding OR proof that at least one ciphertext at every level encrypts
zero, or another protocol construction that preserves both aggregate curves.
