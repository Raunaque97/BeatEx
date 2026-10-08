# BeatEx trader WASM prover

This crate exposes one `TraderProver` WebAssembly runtime for the proof
families required by a trader frontend:

- balance deposit and public withdrawal;
- order submission;
- collateral release (cancellation/refund) and release + replacement;
- settlement into the encrypted balance and settlement + rollover order.

Queue folding, aggregate publication, and the confidential Token-2022 claim
circuit are intentionally excluded.

The runtime lazily accepts four Arkworks canonical-uncompressed proving keys. It
does not generate parameters in the browser. The low-level loader assumes each
key has already passed the manifest's byte-length and SHA-256 checks and skips
redundant curve validation; use `@beatex/prover-wasm` for that authenticated
loading path. Call synchronous proof methods from a dedicated Web Worker so
multi-second proving does not block the UI.

Build the WebAssembly package with `wasm-pack` (or `wasm-bindgen-cli`) from this
directory. Generate key artifacts matching the repository's deterministic
Devnet verification keys with:

```sh
cargo run --release --features devnet-key-export \
  --bin export-devnet-keys -- /path/to/new/output-directory \
  /path/to/beatex_trader_wasm_prover_bg.wasm
```

The exporter refuses to overwrite any expected artifact and writes a manifest
consumable by `@beatex/prover-wasm`, containing byte lengths and SHA-256 hashes
for the runtime and every proving key.
