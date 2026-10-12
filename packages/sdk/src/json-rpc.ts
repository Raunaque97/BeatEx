import type {
  AccountInfo,
  BeatExRpc,
  Commitment,
  ConfirmTransactionOptions,
  SendTransactionOptions,
  SimulationResult,
} from "./boundaries/rpc.js";
import type {
  Address,
  ConfirmedTransaction,
  TransactionSignature,
} from "./boundaries/primitives.js";

export interface JsonRpcBeatExOptions {
  readonly fetch?: typeof globalThis.fetch;
  readonly confirmationPollMs?: number;
  readonly confirmationTimeoutMs?: number;
  /** Retries for rate-limited (429), unavailable (5xx), or unreachable RPC calls. */
  readonly retries?: number;
  readonly retryBaseMs?: number;
}

/**
 * A transaction whose outcome is known: it executed and failed, or its
 * blockhash expired without it landing. Either way it can never apply.
 */
export class TransactionOutcomeError extends Error {
  constructor(
    message: string,
    readonly outcome: "failed" | "expired",
    options?: { readonly cause?: unknown },
  ) {
    super(message, options);
    this.name = "TransactionOutcomeError";
  }
}

const RETRYABLE_HTTP_STATUS = new Set([429, 500, 502, 503, 504]);
/** Check block height (for expiry) at most this often while confirming. */
const EXPIRY_CHECK_MS = 5_000;

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => globalThis.setTimeout(resolve, ms));
}

interface JsonRpcEnvelope<T> {
  readonly result?: T;
  readonly error?: { readonly code: number; readonly message: string; readonly data?: unknown };
}

interface RpcAccountValue {
  readonly data: readonly [string, "base64"];
  readonly executable: boolean;
  readonly lamports: number;
  readonly owner: string;
  readonly rentEpoch?: number;
}

function integerToBigInt(value: number, label: string): bigint {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`Solana RPC returned an unsafe ${label}`);
  }
  return BigInt(value);
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return btoa(binary);
}

function base64ToBytes(encoded: string): Uint8Array {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function decodeAccount(value: RpcAccountValue | null): AccountInfo | null {
  if (value === null) return null;
  if (value.data[1] !== "base64") throw new Error("RPC returned a non-base64 account");
  return {
    owner: value.owner,
    lamports: integerToBigInt(value.lamports, "lamport value"),
    executable: value.executable,
    ...(value.rentEpoch === undefined || !Number.isSafeInteger(value.rentEpoch)
      ? {}
      : { rentEpoch: integerToBigInt(value.rentEpoch, "rent epoch") }),
    data: base64ToBytes(value.data[0]),
  };
}

/** Browser-safe Solana JSON-RPC adapter with no Node.js dependencies. */
export class JsonRpcBeatExRpc implements BeatExRpc {
  readonly endpoint: string;
  readonly #fetch: typeof globalThis.fetch;
  readonly #pollMs: number;
  readonly #timeoutMs: number;
  readonly #retries: number;
  readonly #retryBaseMs: number;
  #requestId = 0;

  constructor(endpoint: string, options: JsonRpcBeatExOptions = {}) {
    const parsed = new URL(endpoint);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
      throw new Error("Solana RPC endpoint must use HTTP or HTTPS");
    }
    this.endpoint = parsed.toString();
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.#pollMs = options.confirmationPollMs ?? 1_000;
    this.#timeoutMs = options.confirmationTimeoutMs ?? 90_000;
    this.#retries = options.retries ?? 3;
    this.#retryBaseMs = options.retryBaseMs ?? 500;
  }

  /** Exponential backoff, or the server's Retry-After when it sends one (capped). */
  #retryDelay(attempt: number, response?: Response): number {
    const retryAfter = Number(response?.headers.get("retry-after"));
    if (Number.isFinite(retryAfter) && retryAfter > 0) return Math.min(retryAfter * 1_000, 5_000);
    return this.#retryBaseMs * 2 ** attempt;
  }

  async #call<T>(method: string, params: readonly unknown[]): Promise<T> {
    // Every BeatEx RPC call is safe to repeat: reads, simulations, and
    // re-sending identical signed bytes (deduplicated by signature).
    let response: Response;
    for (let attempt = 0; ; attempt += 1) {
      try {
        response = await this.#fetch(this.endpoint, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ jsonrpc: "2.0", id: ++this.#requestId, method, params }),
        });
      } catch (error) {
        if (attempt >= this.#retries) throw error;
        await sleep(this.#retryDelay(attempt));
        continue;
      }
      if (RETRYABLE_HTTP_STATUS.has(response.status) && attempt < this.#retries) {
        await sleep(this.#retryDelay(attempt, response));
        continue;
      }
      break;
    }
    if (!response.ok) throw new Error(`Solana RPC ${method} returned HTTP ${response.status}`);
    const envelope = (await response.json()) as JsonRpcEnvelope<T>;
    if (envelope.error !== undefined) {
      const error = new Error(`Solana RPC ${method}: ${envelope.error.message}`);
      Object.assign(error, { code: envelope.error.code, data: envelope.error.data });
      throw error;
    }
    if (!("result" in envelope)) throw new Error(`Solana RPC ${method} omitted its result`);
    return envelope.result as T;
  }

  async getAccount(address: Address, commitment: Commitment = "confirmed"): Promise<AccountInfo | null> {
    const response = await this.#call<{ readonly value: RpcAccountValue | null }>("getAccountInfo", [
      address,
      { commitment, encoding: "base64" },
    ]);
    return decodeAccount(response.value);
  }

  async getMultipleAccounts(
    addresses: readonly Address[],
    commitment: Commitment = "confirmed",
  ): Promise<readonly (AccountInfo | null)[]> {
    if (addresses.length === 0) return [];
    const response = await this.#call<{ readonly value: readonly (RpcAccountValue | null)[] }>(
      "getMultipleAccounts",
      [addresses, { commitment, encoding: "base64" }],
    );
    return response.value.map(decodeAccount);
  }

  async getSlot(commitment: Commitment = "confirmed"): Promise<bigint> {
    return integerToBigInt(await this.#call<number>("getSlot", [{ commitment }]), "slot");
  }

  async getLatestBlockhash(commitment: Commitment = "confirmed") {
    const response = await this.#call<{
      readonly value: { readonly blockhash: string; readonly lastValidBlockHeight: number };
    }>("getLatestBlockhash", [{ commitment }]);
    return {
      blockhash: response.value.blockhash,
      lastValidBlockHeight: integerToBigInt(
        response.value.lastValidBlockHeight,
        "last valid block height",
      ),
    };
  }

  async simulateTransaction(transaction: Uint8Array): Promise<SimulationResult> {
    const response = await this.#call<{
      readonly value: {
        readonly err: unknown | null;
        readonly logs?: readonly string[] | null;
        readonly unitsConsumed?: number;
        readonly returnData?: { readonly data: readonly [string, "base64"] } | null;
      };
    }>("simulateTransaction", [
      bytesToBase64(transaction),
      { encoding: "base64", sigVerify: false, commitment: "confirmed" },
    ]);
    return {
      err: response.value.err,
      logs: response.value.logs ?? [],
      ...(response.value.unitsConsumed === undefined
        ? {}
        : { unitsConsumed: integerToBigInt(response.value.unitsConsumed, "compute-unit count") }),
      ...(response.value.returnData?.data[0] === undefined
        ? {}
        : { returnData: base64ToBytes(response.value.returnData.data[0]) }),
    };
  }

  async sendRawTransaction(
    signedTransaction: Uint8Array,
    options: SendTransactionOptions = {},
  ): Promise<TransactionSignature> {
    return this.#call<string>("sendTransaction", [
      bytesToBase64(signedTransaction),
      {
        encoding: "base64",
        skipPreflight: options.skipPreflight ?? false,
        preflightCommitment: options.preflightCommitment ?? "confirmed",
        ...(options.maxRetries === undefined ? {} : { maxRetries: options.maxRetries }),
      },
    ]);
  }

  async confirmTransaction(
    signature: TransactionSignature,
    options: ConfirmTransactionOptions = {},
  ): Promise<ConfirmedTransaction> {
    const commitment = options.commitment ?? "confirmed";
    const deadline = Date.now() + this.#timeoutMs;
    let nextExpiryCheck = 0;
    let lastError: unknown;
    while (Date.now() < deadline) {
      try {
        const status = await this.#signatureStatus(signature);
        if (status !== null) {
          if (status.err !== null) {
            throw new TransactionOutcomeError(`transaction ${signature} failed`, "failed", {
              cause: status.err,
            });
          }
          const observed = status.confirmationStatus;
          if (observed === "finalized" || (commitment === "confirmed" && observed === "confirmed")) {
            return {
              signature,
              slot: integerToBigInt(status.slot, "confirmation slot"),
              confirmationStatus: observed,
            };
          }
        } else if (options.lastValidBlockHeight !== undefined && Date.now() >= nextExpiryCheck) {
          nextExpiryCheck = Date.now() + EXPIRY_CHECK_MS;
          const blockHeight = integerToBigInt(
            await this.#call<number>("getBlockHeight", [{ commitment }]),
            "block height",
          );
          if (blockHeight > options.lastValidBlockHeight) {
            // It may have landed between the two calls; look once more.
            if ((await this.#signatureStatus(signature)) === null) {
              throw new TransactionOutcomeError(
                `transaction ${signature} expired before confirmation`,
                "expired",
              );
            }
            // It landed after all; the next poll reads its status.
          }
        }
      } catch (error) {
        if (error instanceof TransactionOutcomeError) throw error;
        // A transient RPC failure says nothing about the transaction; keep waiting.
        lastError = error;
      }
      await sleep(this.#pollMs);
    }
    throw new Error(`timed out confirming transaction ${signature}`, { cause: lastError });
  }

  async #signatureStatus(signature: TransactionSignature): Promise<{
    readonly err: unknown | null;
    readonly slot: number;
    readonly confirmationStatus?: "processed" | "confirmed" | "finalized";
  } | null> {
    const response = await this.#call<{
      readonly value: readonly ({
        readonly err: unknown | null;
        readonly slot: number;
        readonly confirmationStatus?: "processed" | "confirmed" | "finalized";
      } | null)[];
    }>("getSignatureStatuses", [[signature], { searchTransactionHistory: true }]);
    return response.value[0] ?? null;
  }
}

export function createBeatExRpc(endpoint: string, options?: JsonRpcBeatExOptions): BeatExRpc {
  return new JsonRpcBeatExRpc(endpoint, options);
}
