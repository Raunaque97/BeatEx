# Demo environment

The demo pair uses two purpose-built Token-2022 mints:

| Symbol | Embedded name | Decimals | Initial demo supply |
| --- | --- | ---: | ---: |
| `tBTC` | BeatEx Test Bitcoin | 8 | 100 |
| `tUSD` | BeatEx Test USD | 6 | 1,000,000 |

Both mints enable embedded Token-2022 metadata and confidential transfers with
automatic approval. They are test assets with no claim on BTC or USD.

## Toolchain

The repository pins Anchor 1.1.2 and Solana/Agave 3.1.10 in `Anchor.toml` for
reproducible program builds. That is an intentional matched build pair. The
mint script requires `spl-token` 5.6.1 or newer because current Devnet rejects
the public-key-validity proof produced by 5.5.0. The newer interface includes
both embedded metadata and confidential-transfer commands.

Check the machine without installing anything:

```sh
scripts/demo/check-toolchain.sh
```

The pinned Anchor and Solana/Agave toolchains are now installed on the current
machine. Run the checker before a build because shell `PATH` configuration can
still differ between terminals.

## Preview the mint plan

This performs no filesystem or network writes:

```sh
scripts/demo/setup-test-tokens.sh --cluster devnet --dry-run
```

## Local validator

Start a validator in another terminal, then run the setup with an explicit
wallet. The script never changes the user's global Solana configuration.

```sh
solana-test-validator --reset

scripts/demo/setup-test-tokens.sh \
  --cluster localnet \
  --wallet /absolute/path/to/demo-wallet.json
```

The mint keys and an `addresses.tsv` manifest are saved in `.demo/localnet/`.
Rerunning the command retains the same mint addresses, creates missing token
accounts, and only creates the initial supply while the mint supply is zero.

The October 2026 local checkpoint deployed program
`2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd` and recreated the retained demo
mints at:

| Symbol | Local mint | Owner token account |
| --- | --- | --- |
| `tBTC` | `DY4Xye68Kt57QBwfBDWPqBimDnSv2zcqtiryvh6AFHvr` | `EVkt9rzKdcnYqBsqD6ewkWqnHTGrMWx2PmvZbNvQJaog` |
| `tUSD` | `2RNCMjSs6zQM3gz36wgqB32TGLsinVbzspans9YKvHnC` | `JST1D6cC775bqegoR3Cvx51pXMwHerBDEjMp9fcTLFP` |

These addresses are reproducible only while the ignored keypairs under
`.demo/localnet/` and the BeatEx program keypair are retained.

## Devnet

Fund the wallet with Devnet SOL, then run:

```sh
solana airdrop 2 WALLET_ADDRESS --url devnet

scripts/demo/setup-test-tokens.sh \
  --cluster devnet \
  --wallet /absolute/path/to/demo-wallet.json
```

Devnet keys and addresses are isolated under `.demo/devnet/`. The script
explicitly refuses any cluster other than localnet or Devnet.

The October 2026 Devnet checkpoint deployed BeatEx and created:

| Item | Devnet address |
| --- | --- |
| BeatEx program | `2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd` |
| tBTC mint | `33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm` |
| tBTC owner account | `AELxK7o5px97XjhMYuEBQunMBXTCQUebqbyzeqEvcnoJ` |
| tUSD mint | `7Fg3pJw2stGKi7WTQRYNsWYqYrFcuwziD8C5HpH9s9Ng` |
| tUSD owner account | `8fRxn1rXBtJoBSVNT5CiVXQZzBdib5CAbBJ1da2gH6Up` |

The demo reference oracle is Pyth's sponsored BTC/USD shard-zero price-feed
account, which is available on Solana Devnet:

| Item | Value |
| --- | --- |
| Price-feed account | `4cSM2e6rvbGQUFiJbqytoVMi5GgghSMr8LwVrT9VPSPo` |
| Feed ID | `e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43` |
| Receiver owner | `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` |

The account's presence and receiver ownership were checked directly against
Devnet. BeatEx does not need to deploy its own oracle for the named test assets;
the program interprets tBTC/tUSD as the demo BTC/USD market. Its market config
uses `oracle_exponent_adjustment = -2` to convert 8-decimal tBTC and 6-decimal
tUSD smallest units before rounding to the configured tick.

Before the public demo, replace the default metadata URI with a hosted JSON
base URI:

```sh
scripts/demo/setup-test-tokens.sh \
  --cluster devnet \
  --wallet /absolute/path/to/demo-wallet.json \
  --metadata-base-uri https://demo.example/tokens
```

The local integration suite uses the official Token-2022 v11.0.0 SBF artifact.
Native confidential proof execution remains an integration gate that must be
tested on the chosen cluster rather than inferred from host or LiteSVM tests.

Devnet token-account configuration was validated with `spl-token` 5.6.1. The
setup script checks this minimum before creating any new state.

## Build and deploy BeatEx

For the locally validated v0 build and deployment:

```sh
scripts/demo/deploy-beatex.sh \
  --cluster localnet \
  --wallet /absolute/path/to/demo-wallet.json
```

The compiled `declare_id!`, both entries in `Anchor.toml`, and the program
keypair must resolve to the same address. The local checkpoint explicitly
verifies this before deployment and keeps its ignored backup at
`.demo/program/beatex-keypair.json`. For Devnet, pass `--cluster devnet`, use a
funded isolated demo wallet, and retain the program keypair. The deployment
script deliberately refuses mainnet.

The current program uses large v1 transactions for queued orders. The v1
transaction feature is active on Devnet, but the eventual demo client must send
serialized transactions with base64 encoding and advertise transaction version
1 support. Legacy and v0 clients keep the old 1,232-byte ceiling. Do not assume
that the build-pinned 3.1.10 `solana-test-validator` reproduces this newer
runtime feature; use Devnet or a v1-capable current validator for the complete
large-transaction flow.

Deployment alone is not the end-to-end demo. The next client/runbook step must
consume `.demo/devnet/addresses.tsv`, initialize the BeatEx market with the
`tBTC` and `tUSD` mint addresses, create the confidential vault accounts, and
exercise the real deposit, order, fold, decrypt, settle, and claim path.
