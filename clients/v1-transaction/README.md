# Solana v1 transaction sender

`send.mjs` signs and confirms one already-serialized BeatEx instruction in a
Solana version-1 transaction. Version 1 raises the transaction limit to 4,096
bytes, which is required by the 17-level encrypted-order payload.

The sender deliberately accepts an instruction bundle instead of duplicating
Anchor instruction encoding. A caller supplies the ordered account metas and
base64 instruction data:

```json
{
  "keypair": "/absolute/path/to/owner.json",
  "signerKeypairs": ["/absolute/path/to/additional-signer.json"],
  "rpcUrl": "https://api.devnet.solana.com",
  "programId": "2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd",
  "accounts": [
    {"address": "...", "writable": true, "signer": true}
  ],
  "dataBase64": "...",
  "computeUnitLimit": 1400000,
  "loadedAccountsDataSizeLimit": 524288,
  "heapSize": 262144,
  "priorityFeeLamports": 0
}
```

Install and validate locally:

```sh
cd clients/v1-transaction
npm ci
npm run check
printf '%s' "$INSTRUCTION_BUNDLE" | node send.mjs --check-only
```

Remove `--check-only` to read the signer, fetch a recent blockhash, sign, and
confirm the transaction. `keypair` may also point at the short-lived session
descriptor created by the native `beatex` client:

```json
{
  "version": 1,
  "type": "browser-wallet",
  "address": "...",
  "bridgeUrl": "http://127.0.0.1:49152",
  "sessionToken": "..."
}
```

In that mode the sender compiles and locally pre-signs the transaction, asks
the loopback bridge to obtain Phantom's v1 signature, and rejects the result if
the compiled message changed or any required signature is missing. The bridge
URL must be loopback HTTP and the bearer token is valid only for the running
client process. The descriptor is deleted when that process exits.

The sender supports additional instruction signers via `signerKeypairs` and
rejects missing signers, non-canonical base64, invalid resource limits, and
encoded transactions above 4,096 bytes.

The order/fold/claim clients remain responsible for reading protocol accounts,
generating proofs, serializing the Anchor instruction, and supplying the exact
account order. This module owns only the version-1 signing and transport layer.
