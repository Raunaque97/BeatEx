# Baby-Jubjub SBF direct-aggregation spike

This isolated crate measures the SBF work required to aggregate one 17-level
order when each level has separate buy and sell ElGamal ciphertexts:

```text
17 levels × 2 sides × 2 curve points = 68 point additions
```

The order sends compressed 32-byte points. The benchmark executes 68
compressed-point recoveries and 68 mixed extended-coordinate additions. The
aggregate account stores four field coordinates per point (8,704 bytes total),
so existing aggregate points do not need to be decompressed or normalized on
each order.

The test reports two measurements: (1) 68 additions with an already
materialized affine point, and (2) 68 compressed-point recoveries plus those
same additions. The second path deliberately repeats one input point 68 times
to fit the legacy transaction object used by LiteSVM. This changes payload
size, but not the SBF curve work being measured.

Build and run with the Solana platform tools:

```sh
cargo build-sbf --manifest-path crates/babyjub-sbf-spike/Cargo.toml --sbf-stack-size 32768
cargo test --manifest-path crates/babyjub-sbf-spike/Cargo.toml \
  measure_add_only_and_68_decompressions_in_sbf -- --ignored --nocapture
```

This spike excludes Groth16 verification, Poseidon binding, Anchor account
deserialization, and token locking. Those costs must be added to the measured
curve cost before deciding whether direct aggregation fits comfortably inside
one transaction.

## Result

LiteSVM / Agave 3.1 SBF-v0 measurements with a 1,400,000 CU limit:

| Work | CU |
|---|---:|
| 1 addition + raw aggregate state I/O | 23,797 |
| 4 additions | 87,277 |
| 8 additions | 171,917 |
| 16 additions | 341,197 |
| 32 additions | 679,757 |
| 68 additions | exceeds 1,400,000 |
| 1 Arkworks compressed decode + addition | 1,123,011 |
| 1 modexp-assisted compressed decode + addition | 498,909 |
| 2 modexp-assisted compressed decodes + additions | 995,181 |
| 4 modexp-assisted compressed decodes + additions | exceeds 1,400,000 |

The aggregate uses raw Arkworks Montgomery limbs, so the addition figures do
not pay canonical field conversion for stored points. The slope from 8 through
32 additions is approximately 21,160 CU per point; 68 additions extrapolate to
about 1.44M CU before Groth16 verification.

Baby-Jubjub's base field has 2-adicity 28. Consequently, compressed-point
recovery needs a Tonelli-Shanks-style square root rather than one fixed
exponentiation. `sol_big_mod_exp` makes inversion and the two large initial
exponentiations cheap, but the remaining root-of-unity correction still costs
hundreds of field squarings for representative points. This is why even the
syscall-assisted decoder costs roughly 475K CU before the point addition.

These results rule out 68 compressed Baby-Jubjub points plus Groth16
verification in one transaction with this representation. Uncompressed input
does not solve it: 68 affine points alone require 4,352 bytes, already above
the 4,096-byte v1 transaction ceiling before the proof and message overhead.
