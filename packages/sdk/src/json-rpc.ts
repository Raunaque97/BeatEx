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
  #requestId = 0;

  constructor(endpoint: string, options: JsonRpcBeatExOptions = {}) {
    const parsed = new URL(endpoint);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
      throw new Error("Solana RPC endpoint must use HTTP or HTTPS");
    }
    this.endpoint = parsed.toString();
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.#pollMs = options.confirmationPollMs ?? 600;
    this.#timeoutMs = options.confirmationTimeoutMs ?? 60_000;
  }

  async #call<T>(method: string, params: readonly unknown[]): Promise<T> {
    const response = await this.#fetch(this.endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: ++this.#requestId, method, params }),
    });
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
    }>("simulateTransaction", [bytesToBase64(transaction), { encoding: "base64", sigVerify: false }]);
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
    while (Date.now() < deadline) {
      const response = await this.#call<{
        readonly value: readonly ({
          readonly err: unknown | null;
          readonly slot: number;
          readonly confirmationStatus?: "processed" | "confirmed" | "finalized";
        } | null)[];
      }>("getSignatureStatuses", [[signature], { searchTransactionHistory: true }]);
      const status = response.value[0];
      if (status !== null && status !== undefined) {
        if (status.err !== null) {
          throw Object.assign(new Error(`transaction ${signature} failed`), { cause: status.err });
        }
        const observed = status.confirmationStatus;
        if (observed === "finalized" || (commitment === "confirmed" && observed === "confirmed")) {
          return {
            signature,
            slot: integerToBigInt(status.slot, "confirmation slot"),
            confirmationStatus: observed,
          };
        }
      }
      if (options.lastValidBlockHeight !== undefined) {
        const blockHeight = integerToBigInt(
          await this.#call<number>("getBlockHeight", [{ commitment }]),
          "block height",
        );
        if (blockHeight > options.lastValidBlockHeight) {
          throw new Error(`transaction ${signature} expired before confirmation`);
        }
      }
      await new Promise<void>((resolve) => globalThis.setTimeout(resolve, this.#pollMs));
    }
    throw new Error(`timed out confirming transaction ${signature}`);
  }
}

export function createBeatExRpc(endpoint: string, options?: JsonRpcBeatExOptions): BeatExRpc {
  return new JsonRpcBeatExRpc(endpoint, options);
}
