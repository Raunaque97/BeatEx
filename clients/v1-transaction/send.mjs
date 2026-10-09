#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import process from "node:process";
import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  assertIsTransactionWithBlockhashLifetime,
  assertIsFullySignedTransaction,
  compileTransaction,
  createKeyPairSignerFromBytes,
  createSolanaRpc,
  createSolanaRpcSubscriptions,
  createTransactionMessage,
  getSignatureFromTransaction,
  getTransactionDecoder,
  getTransactionEncoder,
  partiallySignTransactionWithSigners,
  pipe,
  sendAndConfirmTransactionFactory,
  setTransactionMessageConfig,
  setTransactionMessageFeePayer,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
} from "@solana/kit";

const DEFAULT_RPC_URL = "https://api.devnet.solana.com";
const MAX_V1_BYTES = 4096;

function usage() {
  console.error(`Usage: send.mjs [--check-only]

Reads one JSON instruction bundle from stdin:
{
  "keypair": "/absolute/path/to/owner.json-or-browser-session.json",
  "signerKeypairs": ["/absolute/path/to/additional-signer.json"],
  "rpcUrl": "https://api.devnet.solana.com",
  "instructions": [{
    "programId": "...",
    "accounts": [{"address":"...","writable":true,"signer":true}],
    "dataBase64": "..."
  }],
  "computeUnitLimit": 1400000,
  "loadedAccountsDataSizeLimit": 524288,
  "heapSize": 262144,
  "priorityFeeLamports": 0
}

The fee payer and any signerKeypairs may be marked as signers. --check-only
validates the bundle without reading keypairs or contacting RPC.`);
}

function timingSafeEqual(left, right) {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    difference |= left[index] ^ right[index];
  }
  return difference === 0;
}

function validateBrowserSession(value, path) {
  if (value?.version !== 1 || value?.type !== "browser-wallet") {
    throw new Error(`wallet file is neither a Solana keypair nor a BeatEx browser session: ${path}`);
  }
  const walletAddress = address(value.address);
  const bridgeUrl = new URL(value.bridgeUrl);
  if (bridgeUrl.protocol !== "http:" ||
      (bridgeUrl.hostname !== "127.0.0.1" && bridgeUrl.hostname !== "localhost")) {
    throw new Error("browser wallet bridge must use loopback HTTP");
  }
  if (bridgeUrl.username || bridgeUrl.password || bridgeUrl.search || bridgeUrl.hash) {
    throw new Error("browser wallet bridge URL must not contain credentials, query, or fragment");
  }
  if (typeof value.sessionToken !== "string" || !/^[A-Za-z0-9_-]{32,}$/.test(value.sessionToken)) {
    throw new Error("browser wallet session token is invalid");
  }
  return {
    kind: "browser",
    address: walletAddress,
    bridgeUrl: bridgeUrl.toString().replace(/\/$/, ""),
    sessionToken: value.sessionToken,
  };
}

async function readWallet(path) {
  const value = JSON.parse(await readFile(path, "utf8"));
  if (Array.isArray(value)) {
    const keypairBytes = Uint8Array.from(value);
    if (keypairBytes.length !== 64) {
      throw new Error(`Solana keypair JSON must contain 64 bytes: ${path}`);
    }
    return { kind: "keypair", signer: await createKeyPairSignerFromBytes(keypairBytes) };
  }
  return validateBrowserSession(value, path);
}

async function signWithBrowser(session, transaction, label) {
  const encoded = new Uint8Array(getTransactionEncoder().encode(transaction));
  const response = await fetch(`${session.bridgeUrl}/api/sign-transaction`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${session.sessionToken}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      transactionBase64: Buffer.from(encoded).toString("base64"),
      chain: "solana:devnet",
      label,
    }),
    signal: AbortSignal.timeout(180_000),
  });
  const result = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(result.error ?? `browser wallet bridge returned HTTP ${response.status}`);
  }
  if (typeof result.signedTransactionBase64 !== "string") {
    throw new Error("browser wallet bridge returned no signed transaction");
  }
  const signedBytes = Buffer.from(result.signedTransactionBase64, "base64");
  if (signedBytes.toString("base64") !== result.signedTransactionBase64) {
    throw new Error("browser wallet bridge returned non-canonical base64");
  }
  const signed = getTransactionDecoder().decode(signedBytes);
  if (!timingSafeEqual(signed.messageBytes, transaction.messageBytes)) {
    throw new Error("wallet modified the BeatEx transaction message");
  }
  assertIsFullySignedTransaction(signed);
  return Object.freeze({
    ...signed,
    lifetimeConstraint: transaction.lifetimeConstraint,
  });
}

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

function assertInteger(value, name, minimum, maximum) {
  if (!Number.isInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} must be an integer in [${minimum}, ${maximum}]`);
  }
}

function validateInstruction(instruction, name) {
  if (!instruction || typeof instruction !== "object") throw new Error(`${name} is invalid`);
  address(instruction.programId);
  if (!Array.isArray(instruction.accounts)) {
    throw new Error(`${name}.accounts must be an array`);
  }
  if (instruction.accounts.length > 63) {
    throw new Error(`${name} has more than 63 instruction accounts`);
  }
  for (const [index, account] of instruction.accounts.entries()) {
    if (!account || typeof account !== "object") {
      throw new Error(`${name}.accounts[${index}] is invalid`);
    }
    address(account.address);
    if (typeof account.writable !== "boolean" || typeof account.signer !== "boolean") {
      throw new Error(
        `${name}.accounts[${index}] must specify boolean writable and signer fields`,
      );
    }
  }
  if (typeof instruction.dataBase64 !== "string") {
    throw new Error(`${name}.dataBase64 is required`);
  }
  const data = Buffer.from(instruction.dataBase64, "base64");
  if (data.toString("base64") !== instruction.dataBase64) {
    throw new Error(`${name}.dataBase64 is not canonical base64`);
  }
  return { ...instruction, data };
}

function validateBundle(bundle, checkOnly) {
  if (!bundle || typeof bundle !== "object") throw new Error("bundle must be an object");
  const rawInstructions = bundle.instructions ?? [{
    programId: bundle.programId,
    accounts: bundle.accounts,
    dataBase64: bundle.dataBase64,
  }];
  if (!Array.isArray(rawInstructions) || rawInstructions.length === 0) {
    throw new Error("instructions must be a non-empty array");
  }
  if (rawInstructions.length > 64) throw new Error("at most 64 instructions are allowed");
  const instructions = rawInstructions.map((instruction, index) =>
    validateInstruction(instruction, `instructions[${index}]`),
  );
  assertInteger(bundle.computeUnitLimit ?? 1_400_000, "computeUnitLimit", 1, 1_400_000);
  assertInteger(
    bundle.loadedAccountsDataSizeLimit ?? 512 * 1024,
    "loadedAccountsDataSizeLimit",
    1,
    64 * 1024 * 1024,
  );
  assertInteger(bundle.heapSize ?? 256 * 1024, "heapSize", 32 * 1024, 256 * 1024);
  const priorityFeeLamports = bundle.priorityFeeLamports ?? 0;
  if (!Number.isSafeInteger(priorityFeeLamports) || priorityFeeLamports < 0) {
    throw new Error("priorityFeeLamports must be a non-negative safe integer");
  }
  if (!checkOnly && (typeof bundle.keypair !== "string" || bundle.keypair.length === 0)) {
    throw new Error("keypair is required when sending");
  }
  if (bundle.signerKeypairs !== undefined &&
      (!Array.isArray(bundle.signerKeypairs) ||
       bundle.signerKeypairs.some((path) => typeof path !== "string" || path.length === 0))) {
    throw new Error("signerKeypairs must be an array of non-empty paths");
  }
  return instructions;
}

function subscriptionsUrl(rpcUrl) {
  const url = new URL(rpcUrl);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

async function main() {
  if (process.argv.includes("-h") || process.argv.includes("--help")) {
    usage();
    return;
  }
  const checkOnly = process.argv.includes("--check-only");
  const source = await readStdin();
  const bundle = JSON.parse(source);
  const validatedInstructions = validateBundle(bundle, checkOnly);
  if (checkOnly) {
    console.log(JSON.stringify({
      valid: true,
      instructionCount: validatedInstructions.length,
      instructionDataBytes: validatedInstructions.reduce(
        (total, instruction) => total + instruction.data.length,
        0,
      ),
    }));
    return;
  }

  async function readSigner(path) {
    const keypairBytes = Uint8Array.from(JSON.parse(await readFile(path, "utf8")));
    if (keypairBytes.length !== 64) {
      throw new Error(`Solana keypair JSON must contain 64 bytes: ${path}`);
    }
    return createKeyPairSignerFromBytes(keypairBytes);
  }
  const wallet = await readWallet(bundle.keypair);
  const additionalSigners = await Promise.all((bundle.signerKeypairs ?? []).map(readSigner));
  const payerAddress = wallet.kind === "keypair" ? wallet.signer.address : wallet.address;
  const signers = new Map();
  if (wallet.kind === "keypair") signers.set(wallet.signer.address, wallet.signer);
  for (const signer of additionalSigners) {
    if (signer.address === payerAddress || signers.has(signer.address)) {
      throw new Error(`duplicate signer ${signer.address}`);
    }
    signers.set(signer.address, signer);
  }
  const rpcUrl = bundle.rpcUrl ?? DEFAULT_RPC_URL;
  const rpc = createSolanaRpc(rpcUrl);
  const rpcSubscriptions = createSolanaRpcSubscriptions(subscriptionsUrl(rpcUrl));
  const { value: latestBlockhash } = await rpc.getLatestBlockhash({ commitment: "confirmed" }).send();

  const instructions = validatedInstructions.map((instruction) => {
    const accounts = instruction.accounts.map((accountMeta) => {
      const signer = accountMeta.signer;
      const accountSigner = signer ? signers.get(accountMeta.address) : undefined;
      const isBrowserSigner = wallet.kind === "browser" && accountMeta.address === wallet.address;
      if (signer && !accountSigner && !isBrowserSigner) {
        throw new Error(`missing keypair for signer ${accountMeta.address}`);
      }
      const role = accountMeta.writable
        ? signer
          ? AccountRole.WRITABLE_SIGNER
          : AccountRole.WRITABLE
        : signer
          ? AccountRole.READONLY_SIGNER
          : AccountRole.READONLY;
      return accountSigner
        ? { address: accountSigner.address, role, signer: accountSigner }
        : { address: address(accountMeta.address), role };
    });
    return {
      programAddress: address(instruction.programId),
      accounts,
      data: new Uint8Array(instruction.data),
    };
  });
  const transactionMessage = pipe(
    createTransactionMessage({ version: 1 }),
    (message) => wallet.kind === "keypair"
      ? setTransactionMessageFeePayerSigner(wallet.signer, message)
      : setTransactionMessageFeePayer(payerAddress, message),
    (message) => setTransactionMessageLifetimeUsingBlockhash(latestBlockhash, message),
    (message) => appendTransactionMessageInstructions(instructions, message),
    (message) =>
      setTransactionMessageConfig(
        {
          computeUnitLimit: bundle.computeUnitLimit ?? 1_400_000,
          loadedAccountsDataSizeLimit: bundle.loadedAccountsDataSizeLimit ?? 512 * 1024,
          heapSize: bundle.heapSize ?? 256 * 1024,
          priorityFeeLamports: BigInt(bundle.priorityFeeLamports ?? 0),
        },
        message,
      ),
  );
  const transaction = wallet.kind === "keypair"
    ? await signTransactionMessageWithSigners(transactionMessage)
    : await partiallySignTransactionWithSigners(additionalSigners, compileTransaction(transactionMessage));
  assertIsTransactionWithBlockhashLifetime(transaction);
  const encoded = getTransactionEncoder().encode(transaction);
  if (encoded.length > MAX_V1_BYTES) {
    throw new Error(`v1 transaction is ${encoded.length} bytes; maximum is ${MAX_V1_BYTES}`);
  }
  const signedTransaction = wallet.kind === "browser"
    ? await signWithBrowser(wallet, transaction, bundle.label ?? "BeatEx transaction")
    : transaction;
  try {
    await sendAndConfirmTransactionFactory({ rpc, rpcSubscriptions })(signedTransaction, {
      commitment: "confirmed",
    });
  } catch (error) {
    try {
      const simulation = await rpc
        .simulateTransaction(
          Buffer.from(getTransactionEncoder().encode(signedTransaction)).toString("base64"), {
          commitment: "confirmed",
          encoding: "base64",
          sigVerify: false,
          })
        .send();
      if (simulation.value.logs) console.error(simulation.value.logs.join("\n"));
      if (simulation.value.unitsConsumed !== undefined) {
        console.error(`simulation units consumed: ${simulation.value.unitsConsumed}`);
      }
    } catch (simulationError) {
      console.error(`diagnostic simulation failed: ${simulationError}`);
    }
    throw error;
  }
  console.log(
    JSON.stringify({
      signature: getSignatureFromTransaction(signedTransaction),
      transactionBytes: getTransactionEncoder().encode(signedTransaction).length,
    }),
  );
}

main().catch((error) => {
  console.error(`v1 transaction failed: ${error instanceof Error ? error.message : String(error)}`);
  if (error && typeof error === "object" && "cause" in error && error.cause) {
    console.error(error.cause);
  }
  process.exitCode = 1;
});
