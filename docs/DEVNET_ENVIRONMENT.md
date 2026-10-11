# Devnet testing environment

The Devnet pair uses two purpose-built Token-2022 mints:

| Symbol | Embedded name | Decimals | Initial test supply |
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

For reproducibility, a project-local CLI can be installed under the ignored
`.devnet/tools/` directory:

```sh
cargo install spl-token-cli --version 5.6.1 --locked --root .devnet/tools
```

The Devnet scripts automatically prefer `.devnet/tools/bin` and the user's
Cargo bin directory when present; they do not alter the global Solana config.

Check the machine without installing anything:

```sh
scripts/devnet/check-toolchain.sh
```

The pinned Anchor and Solana/Agave toolchains are now installed on the current
machine. Run the checker before a build because shell `PATH` configuration can
still differ between terminals.

## Preview the mint plan

This performs no filesystem or network writes:

```sh
scripts/devnet/setup-test-tokens.sh --cluster devnet --dry-run
```

## Local validator

Start a validator in another terminal, then run the setup with an explicit
wallet. The script never changes the user's global Solana configuration.

```sh
solana-test-validator --reset

scripts/devnet/setup-test-tokens.sh \
  --cluster localnet \
  --wallet /absolute/path/to/devnet-wallet.json
```

The mint keys and an `addresses.tsv` manifest are saved in `.devnet/localnet/`.
Rerunning the command retains the same mint addresses, creates missing token
accounts, and only creates the initial supply while the mint supply is zero.

The October 2026 local checkpoint deployed program
`2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd` and recreated the retained test
mints at:

| Symbol | Local mint | Owner token account |
| --- | --- | --- |
| `tBTC` | `DY4Xye68Kt57QBwfBDWPqBimDnSv2zcqtiryvh6AFHvr` | `EVkt9rzKdcnYqBsqD6ewkWqnHTGrMWx2PmvZbNvQJaog` |
| `tUSD` | `2RNCMjSs6zQM3gz36wgqB32TGLsinVbzspans9YKvHnC` | `JST1D6cC775bqegoR3Cvx51pXMwHerBDEjMp9fcTLFP` |

These addresses are reproducible only while the ignored keypairs under
`.devnet/localnet/` and the BeatEx program keypair are retained.

## Devnet

Fund the wallet with Devnet SOL, then run:

```sh
solana airdrop 2 WALLET_ADDRESS --url devnet

scripts/devnet/setup-test-tokens.sh \
  --cluster devnet \
  --wallet /absolute/path/to/devnet-wallet.json
```

Devnet keys and addresses are isolated under `.devnet/devnet/`. The script
explicitly refuses any cluster other than localnet or Devnet.

The following addresses belong to the October 2026 version-1 checkpoint. The
token mints may be reused, but its program build, market, vault configuration,
key epoch, verifier keys, and generated manifests predate the version-2 unit
and linear-bps-grid migration and must not be treated as a current deployment:

| Item | Devnet address |
| --- | --- |
| BeatEx program | `2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd` |
| tBTC mint | `33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm` |
| tBTC owner account | `AELxK7o5px97XjhMYuEBQunMBXTCQUebqbyzeqEvcnoJ` |
| tUSD mint | `7Fg3pJw2stGKi7WTQRYNsWYqYrFcuwziD8C5HpH9s9Ng` |
| tUSD owner account | `8fRxn1rXBtJoBSVNT5CiVXQZzBdib5CAbBJ1da2gH6Up` |
| BeatEx market | `3haCd5iqRJuk9FGVFRdkW6jMnKQL4CH4vgwdpVc8XzxS` |
| Vault authority PDA | `2gbWNfRTH37Nvc3k3FCayaT1FvXysDTF8KKE3SpcWnWN` |
| tBTC vault | `GGjBZSXRVm3yQ5yh1bBSzh9yuzJ7nWeEgfNGGv65LasK` |
| tUSD vault | `4GQapjjphiQiEBt85JuCrAZz1fQfCZJU41PLqS4snBZi` |
| Active key epoch | `AoJ755HzDcFx8VZB3rpotAWRszxv7dCVZYX9WSRe3abe` |

The idempotent setup also created two persistent, confidential-transfer-enabled
market-maker wallets:

| Strategy | Owner | tBTC account | tUSD account | Funding |
| --- | --- | --- | --- | --- |
| Pyth | `GYuK4aof3fS7M1dzYzwLrrdokdQsVieuPfVuEBxjCitq` | `CHSkbKWPe9cMNH8fgxWyzgn1VbZFnnBE1UheeveYzEUr` | `3AFRVARpnqH1XT5uHi329aDmF9P8oVGZx4Cmqy33R2dL` | 1 SOL, 2 tBTC, 50,000 tUSD |
| Binance | `33YCrUGfEjQFv1Le47nQ26aUua3ryBbvs1Z2DfAibip4` | `Cfe7YzTEWp8fGgo2WMVExSaPJEhxzgB1A4XHKGe4DqmA` | `GmikWnVcGbcoFbdYoFTa498Ph2y1qd1z3MryvrPGjzkt` | 1 SOL, 2 tBTC, 50,000 tUSD |

Private keypairs stay under ignored `.devnet/devnet/market-makers/`; the public
manifest is regenerated at `.devnet/devnet/market-makers.tsv`.

Market makers do not accept a local reference price or an absolute tick size.
They discover the slot-derived auction and derive its 17-level price grid from
the snapshotted reference price and basis-point step
before constructing a curve:

Run one maker per terminal. The source is explicit so each process has its own
wallet and retry loop:

```sh
scripts/devnet/run-market-maker.sh --source pyth
scripts/devnet/run-market-maker.sh --source binance
```

Paid RPC credentials stay outside tracked source. Copy
`scripts/devnet/rpc.env.example` to `.devnet/devnet/rpc.env`, set
`BEATEX_RPC_SESSION_URL` to the Helius Devnet endpoint, and put the Alchemy
Devnet endpoint first in `BEATEX_RPC_FALLBACK_URLS` (with the public Devnet
endpoint optional as a final fallback). Each long-running bot keeps stateful
reads and transaction submissions on its primary endpoint, then fails over in
the configured order for a transport failure or HTTP 429. A
deterministic simulation/program error is returned immediately and is never
replayed against another provider. The primary and every fallback must target
the same Solana cluster. Endpoint diagnostics redact Helius query keys and
Alchemy path keys.

The Rust bot prints the maker's private ledger balance every ten auctions by
default. Change that cadence with `--balance-every-auctions N`, or use
`--print-only --once` to inspect one generated intent without submitting it.

The committee bot initializes each Auction account. Market makers wait for that
account and never submit initialization transactions. A locally fetched Pyth
or Binance price influences a maker's desired quantities only; it never
replaces the Auction account's price levels.

The reference oracle is Pyth's sponsored BTC/USD shard-zero price-feed
account, which is available on Solana Devnet:

| Item | Value |
| --- | --- |
| Price-feed account | `4cSM2e6rvbGQUFiJbqytoVMi5GgghSMr8LwVrT9VPSPo` |
| Feed ID | `e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43` |
| Receiver owner | `rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ` |

The legacy checkpoint's 972,376-byte program artifact at
`target/deploy-v0/beatex.so` has
SHA-256
`e8d377290afe956253ba747375977c8ba2e0392239bb8121443085c70725a3e7`.
It was built without test-fixture features after the production artifact passed
the full 17-level submit, replacement-aware fold, cancellation, internal
settlement/refund, and public-withdrawal SBF tests. The upgradeable ProgramData
account has a 973,368-byte
capacity. The deployment transaction is
`B5jApY1XGShb3JUmFVXMUHjqH8DZM4i9wd6PTaW9RdcMrGthbMXXFCWiJuhti5ihzAA7UqmhzACpKdWBQ1AEJnZ`.

The account's presence and receiver ownership were checked directly against
Devnet. BeatEx does not need to deploy its own oracle for the named test assets;
the program interprets tBTC/tUSD as the test BTC/USD market. The replacement
version-2 market configuration is:

```text
base_unit_size             = 100 tBTC atoms = 100 satoshis
quote_unit_size            = 1 tUSD atom = 1 micro-tUSD
grid_radius                = 8
grid_step_bps              = 10
oracle_exponent_adjustment = 0
```

This produces 17 linear offsets from -80 through +80 bps around each auction's
reference price. The Auction snapshots the reference, step, and level count;
all clients derive the same exact half-up-rounded prices from those fields, and
the snapshot is authoritative. The Devnet market allows a
600-second oracle age because the sponsored Devnet feed updates less frequently
than its production counterpart.

Before public Devnet testing, replace the default metadata URI with a hosted JSON
base URI:

```sh
scripts/devnet/setup-test-tokens.sh \
  --cluster devnet \
  --wallet /absolute/path/to/devnet-wallet.json \
  --metadata-base-uri https://devnet.example/tokens
```

The local integration suite uses the official Token-2022 v11.0.0 SBF artifact.
Native confidential proof execution is also validated directly on Devnet. The
proof service uses Solana ZK SDK v7 and
`spl-token-confidential-transfer-proof-generation` 0.6.1; proofs generated with
the older v4 transcript are not compatible with the current native proof
program.

Devnet token-account configuration was validated with `spl-token` 5.6.1. The
setup script checks this minimum before creating any new state.

## Build and deploy BeatEx

### Reproducible build artifacts

Run the repository pipeline after changing the Anchor program:

```sh
pnpm build:contract   # SBF, IDL, generated protocol files, deployment manifest
pnpm build:protocol   # contract artifacts plus Rust/TypeScript package builds
pnpm build:all        # also builds trader WASM assets and the web application
```

The pipeline derives values from `Anchor.toml`,
`programs/beatex/src/state.rs`, and `programs/beatex/src/queue.rs`. It writes
the generated SDK and prover files and records the current IDL hash and any
known market/vault/key addresses in `.devnet/<cluster>/deployment.json`.
`deploy-beatex.sh` runs the contract-only pipeline before deploying, while
`devnetSetup.sh` regenerates the manifest after market initialization. This
keeps frontend, CLI, TUI, prover, and market-maker consumers on the same
program ID and protocol configuration without hand-editing addresses.

For the locally validated v0 build and deployment:

```sh
scripts/devnet/deploy-beatex.sh \
  --cluster localnet \
  --wallet /absolute/path/to/devnet-wallet.json
```

Both this command and the enclosing `devnetSetup.sh` inspect the configured
program ID with `solana program show` before deployment. If it is already
deployed, an interactive run asks before upgrading it. Declining leaves the
current program unchanged. An unattended setup also leaves it unchanged unless
`--redeploy` was supplied after explicit approval. `deploy-beatex.sh` requires
the corresponding lower-level `--allow-redeploy` flag when called directly in
an unattended workflow.

The compiled `declare_id!`, both entries in `Anchor.toml`, and the program
keypair must resolve to the same address. The local checkpoint explicitly
verifies this before deployment and keeps its ignored backup at
`.devnet/program/beatex-keypair.json`. For Devnet, pass `--cluster devnet`, use a
funded isolated Devnet wallet, and retain the program keypair. The deployment
script deliberately refuses mainnet.

BeatEx uses large Solana version-1 transactions for queued orders. The v1
transaction feature is active on Devnet. `clients/v1-transaction/send.mjs`
builds, signs, size-checks, sends, and confirms these 4,096-byte-cap transactions
with explicit compute, loaded-account-data, heap, and priority-fee configuration.
It consumes a client-generated Anchor instruction bundle; proof generation and
account discovery remain outside this transport module. Legacy and v0 clients
keep the old 1,232-byte ceiling. Do not assume that the build-pinned 3.1.10
`solana-test-validator` reproduces this newer runtime feature; use Devnet or a
v1-capable current validator for the complete large-transaction flow.

`scripts/devnet/devnetSetup.sh` now consumes the token manifest, creates and
configures the Token-2022 custody vaults, transfers their ownership to the vault PDA,
initializes the market and committee, and installs every verifier key. The
ignored `.devnet/devnet/market.tsv` manifest records all resulting addresses.
The Devnet market uses a 40-slot order-collection interval (approximately ten
seconds under normal Devnet slot timing). Auction N-1 is folded, decrypted, and
published while auction N collects orders, so auction N+1 can be centered on
N-1's clearing price; publication takes about four seconds, which a 20-slot
window could not fit alongside the traders' 12-slot preparation lead.
It also regenerates `target/idl/beatex.json` with `anchor idl build` and
publishes it with `@solana-program/program-metadata`. The
publication helper fetches the canonical IDL first, skips semantically
identical JSON, and asks before replacing a different or unconfirmed value.
Use `--skip-idl` to omit publication or `--allow-idl-update` only for an
explicitly approved unattended update.
`scripts/devnet/run-market-maker.sh` uses the stateful proof/sign/send adapter
by default; `--print-only` keeps the strategy inspection mode. The separate
`scripts/devnet/run-committee-bot.sh` is a permissionless coordinator: it
observes the temporal schedule without allocating empty auction accounts,
folds queued orders, and publishes aggregate curves when the gates permit it.
Its scheduler reads the overlapping pipeline in one batched RPC snapshot and
launches concurrent workers only for auctions requiring a fold, publication,
or cancellation, so proof work does not block later auction boundaries.
The same adapter provides `fold`,
`publish`, and `cancel` commands plus `settle-balance`,
`refund-balance`, and `withdraw` for the default lifecycle. The older `claim`
and `refund` commands retain the optional confidential Token-2022 exit. Its
rollover path sends one version-1 transaction with
six BeatEx instructions: account preparation, settlement-proof verification,
settlement-statement binding, order-proof verification, order-statement
binding, and the final state mutation. The preparation and binding handlers use
the instructions sysvar to authenticate the complete atomic chain; no reusable
proof receipt or partial rollover state is stored.

`scripts/devnet/run-indexer.sh` runs the auction-history indexer. It follows
the program's event-authority PDA, decodes the auction outcome events, stores
them under `.devnet/<cluster>/indexer`, and serves
`GET /v1/markets/<market>/auctions?limit=60` on `127.0.0.1:8787` by default.
Settled and cancelled auction accounts are compacted on chain, so the web
chart reads finished batches from this API (`NEXT_PUBLIC_BEATEX_INDEXER_URL`)
and only in-flight batches from chain.

## Legacy version-1 settlement checkpoint

The transactions below document the superseded version-1 deployment. They are
retained as historical execution evidence, not as validation of the version-2
unit, grid, transcript, and verifier-key migration.

The default fast-ledger path completed auction 1007 against the deployed
program:

| Step | Transaction | Size | Compute | Fee |
| --- | --- | ---: | ---: | ---: |
| Public deposit | `3PgTBAieLnjHte7cHF5A9SNWtkDjpPjSEY3WG16Y5ise3M39GEJoonWiKiHA4bbNP7MMwvepsk3ZdqBoe3vb9WVZ` | 1,031 bytes | 133,989 CU | 5,000 lamports |
| Order submission | `3wbTSZR2DiHkkTGwZCvq4Nqyc28wjn5zzHcwvwzEuPdB8kCde3J6WKXKmPFrzfErJQhHuJndvWHjoofhKaGDprZk` | 3,136 bytes | 512,747 CU | 5,000 lamports |
| Permissionless fold | `4hLC2UWzLBVmKbF8q1trevU5KevateJmsygYEUg4A8vEyWtF3Ev127zQa3ZCTvKM2Qr8RKXwWPqsMg9pFRsMGZS4` | 2,869 bytes | 1,113,367 CU | 5,000 lamports |
| Publish and clear | `9vqZEdND4SfSPE3UNWmPCDtPQbo4QTjVwc1rT1Em81eqGKak7tDnK3WBe4huX1k4nK7xmZ1bt3ZtUyRqeE2LMkM` | 963 bytes | 267,487 CU | 5,000 lamports |
| Settle to encrypted balance | `5VBQhpgz4GyFuvNXcgm1WFiaQc8pAoLQirfHMKneVLoWokXomxgjhKhXNYzjFouAeExGmtFoxSZpa557FLcX3vSQ` | 1,015 bytes | 154,806 CU | 5,000 lamports |
| Public withdrawal | `5TPKMrSAaEx9rN5proDsMbQLo1bu3LFSSiVE3KTKftenLEuDvoJZB2pmZBzLGC2J44MiMeB2khZKexXweti3GdCT` | 1,031 bytes | 134,005 CU | 5,000 lamports |

This is the frontend-facing path: deposit once, reuse the private balance for
orders and settlements, then withdraw only when the user wants public tokens.
No native Token-2022 proof-context transactions are required. Public deposit
and withdrawal amounts are visible; order and settlement values stay private.

The earlier confidential Token-2022 exit also remains validated:

The October 2026 run completed both terminal paths against the deployed
program:

| Path | Auction | Withdrawal transaction | Size | Compute | Fee |
| --- | ---: | --- | ---: | ---: | ---: |
| Private claim | 18 | `3ZFJANW7MEbkjuKcj8jDSpmTM5f5qqF6djoXuTBzhgjwdm8JYeeFzw8kUSG62tX9EtwCvBRbHP1WQkPz39Tu2KB` | 1,510 bytes | 267,107 CU | 5,000 lamports |
| Cancellation refund | 14 | `2qyK73ank6tkJshMMRtZWYvnpronr3AGJC68JZN2mpim4N7nHjsARVLkx1H5XqmVmCE22aqjLejN7dQ9to6qyFhg` | 1,510 bytes | 267,105 CU | 5,000 lamports |

Each path first created and verified six native proof contexts. Equality,
grouped-validity, and U128 range transactions were 703, 927, and 1,383 bytes
respectively for each asset. Those transactions have two signatures and cost
10,000 lamports each. One 430-byte cleanup transaction closed all six contexts
and returned their rent; every transaction used a zero priority fee. The claim
credited 8 base units and 6,520 quote units. The refund credited its full
private reserves of 7 base units and 5,705 quote units. Both pairs of pending
credits were then applied into the recipients' available confidential balances.

The recurring-liquidity path also completed live:

| Path | Auctions | Transaction | Size | Private payout |
| --- | --- | --- | ---: | ---: |
| Atomic rollover | 528 -> 819 | `3C1j93M4Hc85T5NpzmfJbCCBcyL9wuJ5AgRoYrqH4B8iBcQ8VV5iNCBxme1MAjPLLW2LmBMCLkDu92CVQq49Ck2c` | 4,093 bytes | 9 base, 7,155 quote |
| Repeated atomic rollover | 822 -> 823 | `4UGNJtAXKwEHFFUKTPtw29LeZrjU73BGfzLinKhPXHArPWY1KySYUBhscWgq9XNXtTkTLxe1tXnsgsh1Ym8sLJfC` | 4,093 bytes | 9 base, 7,155 quote |

The transaction prepared the next order accounts, verified and bound both
Groth16 statements in isolated instruction heaps, terminally claimed auction
528's order into the private encrypted balance, and submitted auction 819's
next order. All six instructions committed atomically.

The second run started from a new encrypted order in auction 822, folded its
queue event, published its aggregate, settled at price 835, and rolled the
private result into auction 823. Submission consumed 512,745 CU, the one-event
fold consumed 1,113,365 CU, publication and clearing consumed 267,485 CU, and
the rollover consumed 694,958 CU. Each transaction paid 5,000 lamports and used
no priority fee. The corresponding submission, fold, and publication
signatures are `3qaMbomdoiLa8S8qS7o2Q294RQ79W1trPKRZCiCGS8guCQZi6nqAu337NpZ8Lj291KuY2JrGSRFZHVb7xYqobCJo`,
`5L9wkpMQYShK9jFgP2v1dmfiWu7or34Dhd57E9r51iW9YxmC7YVypFT8oZXT2fqNU1UGmsC7vMKKhRrmaWgRpkJd`,
and `49Tv5gUYpazsrvh9CrgYxXjYUHAi9pwDtv6yskWH7vSMBdJbceGdZxhoeTt5M4NZrMVAMt5ZzLmCMqz8KQ1Fkypk`.

Auction 823 also exercised the same-auction edit path. A 3,618-byte replacement
transaction (`2xhsRi1LH8moSAwxssEDFPoP1Bu373SdbXsHCKAgDFrpnfnjC8vGCNMBCCygp1iPsZBcBbJiMqs72A3JHfiHVz7f`)
consumed 628,662 CU. Both the original and replacement deltas then folded in one
2,902-byte transaction (`672Wwp5jAnvtCwCbEejtnb4hscMr9XhDnum5rDHXPxaM3gMjkAppZg3iBSJu7Z9qr5TEeie7P1afED9zLjWXF75U`)
at 862,979 CU. Publication settled auction 823 at price 835 in
`oq322NVqpru49QM5XLmXinnipvtPbNwCXExpz1EdKzrki9smRRsm4PWe5usbR4y4wEfzCtSqAQcSTVdbPwZi9rn`.
All three transactions paid 5,000 lamports with zero priority fee.
