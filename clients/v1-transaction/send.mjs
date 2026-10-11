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
  createTransactionMessage,
  getSignatureFromTransaction,
  getTransactionDecoder,
  getTransactionEncoder,
  partiallySignTransactionWithSigners,
  pipe,
  sendTransactionWithoutConfirmingFactory,
  setTransactionMessageConfig,
  setTransactionMessageFeePayer,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
} from "@solana/kit";

const DEFAULT_RPC_URL = "https://api.devnet.solana.com";
const MAX_V1_BYTES = 4096;
const TX_COMMITMENT = process.env.BEATEX_TX_COMMITMENT ?? "confirmed";

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

function rpcCandidates(primaryRpcUrl) {
  const candidates = [
    primaryRpcUrl,
    ...(process.env.BEATEX_RPC_FALLBACK_URLS ?? "")
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean),
  ];
  return candidates.filter((candidate, index) => candidates.indexOf(candidate) === index);
}

function rpcLabel(rpcUrl) {
  const url = new URL(rpcUrl);
  let pathname = url.pathname;
  for (const marker of ["/v1/", "/v2/"]) {
    const index = pathname.indexOf(marker);
    if (index >= 0) {
      pathname = `${pathname.slice(0, index + marker.length)}<redacted>`;
      break;
    }
  }
  return `${url.protocol}//${url.host}${pathname}`;
}

function isRetryableRpcError(error) {
  const detail = `${error instanceof Error ? error.message : String(error)} ${
    error && typeof error === "object" && "cause" in error ? String(error.cause) : ""
  }`.toLowerCase();
  return /(?:http error \(429\)|too many requests|fetch failed|network|socket|econn|timed? ?out|timeout|websocket|connection closed)/.test(detail);
}

function isAuctionInitializationRace(error, label) {
  if (!label?.startsWith("initialize auction")) return false;
  const contexts = [error, error?.cause].filter(Boolean);
  return contexts.some((value) => {
    const logs = value?.context?.logs;
    return Array.isArray(logs) && logs.some((line) => line.includes("already in use"));
  });
}

function logRpcFailure(action, rpcUrl, error) {
  console.error(`${action} via ${rpcLabel(rpcUrl)} failed:`);
  console.error(error);
}

async function latestBlockhashWithFailover(rpcUrls) {
  const failures = [];
  for (const rpcUrl of rpcUrls) {
    try {
      const rpc = createSolanaRpc(rpcUrl);
      return await rpc.getLatestBlockhash({ commitment: TX_COMMITMENT }).send();
    } catch (error) {
      failures.push(error);
      logRpcFailure("latest blockhash request", rpcUrl, error);
      if (!isRetryableRpcError(error)) throw error;
    }
  }
  throw new AggregateError(failures, "latest blockhash request failed via all configured RPC URLs");
}

async function diagnoseTransaction(rpc, rpcUrl, signedTransactionBase64) {
  try {
    const simulation = await rpc
      .simulateTransaction(signedTransactionBase64, {
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
    console.error(`diagnostic simulation via ${rpcLabel(rpcUrl)} failed:`);
    console.error(simulationError);
  }
}

function commitmentRank(commitment) {
  return { processed: 0, confirmed: 1, finalized: 2 }[commitment] ?? 1;
}

async function sendAndPoll(rpc, signedTransaction, waitForConfirmation = true) {
  const signature = getSignatureFromTransaction(signedTransaction);
  await sendTransactionWithoutConfirmingFactory({ rpc })(signedTransaction, {
    commitment: TX_COMMITMENT,
    skipPreflight: !waitForConfirmation,
  });
  if (!waitForConfirmation) return;
  const deadline = Date.now() + (TX_COMMITMENT === "processed" ? 8_000 : 30_000);
  while (Date.now() < deadline) {
    const { value: statuses } = await rpc
      .getSignatureStatuses([signature])
      .send();
    const status = statuses[0];
    if (status?.err) throw new Error(`transaction failed on-chain: ${JSON.stringify(status.err)}`);
    if (status?.confirmationStatus && commitmentRank(status.confirmationStatus) >= commitmentRank(TX_COMMITMENT)) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`transaction confirmation timed out after ${TX_COMMITMENT} commitment`);
}

async function sendAndConfirmWithFailover(signedTransaction, rpcUrls, label) {
  const failures = [];
  const signedTransactionBase64 = Buffer.from(
    getTransactionEncoder().encode(signedTransaction),
  ).toString("base64");
  for (const rpcUrl of rpcUrls) {
    let rpc;
    try {
      rpc = createSolanaRpc(rpcUrl);
      // Auction materialization sits on the critical path of the
      // collection window. RPC acceptance plus the caller's account polling
      // is sufficient; waiting here for a second status round-trip can consume
      // the entire auction.
      const waitForConfirmation = !(
        TX_COMMITMENT === "processed" && label?.startsWith("initialize auction")
      );
      await sendAndPoll(rpc, signedTransaction, waitForConfirmation);
      return;
    } catch (error) {
      if (isAuctionInitializationRace(error, label)) return;
      failures.push(error);
      logRpcFailure("transaction submission", rpcUrl, error);
      if (isRetryableRpcError(error)) continue;
      if (rpc) await diagnoseTransaction(rpc, rpcUrl, signedTransactionBase64);
      throw error;
    }
  }
  throw new AggregateError(failures, "transaction submission failed via all configured RPC URLs");
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
  const rpcUrls = rpcCandidates(rpcUrl);
  const { value: latestBlockhash } = await latestBlockhashWithFailover(rpcUrls);

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
  await sendAndConfirmWithFailover(signedTransaction, rpcUrls, bundle.label);
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
