import type {
  BeatExRpc,
  BeatExSigner,
  ProofExecutionOptions,
} from "@beatex/sdk";

/** The transaction phases every action screen shows separately. */
export type TxPhase =
  | "idle"
  | "building"
  | "proving"
  | "simulating"
  | "awaiting-signature"
  | "submitted"
  | "confirmed"
  | "failed";

export interface TxProgress {
  readonly phase: TxPhase;
  /** Which signature of a multi-transaction action is pending (1-based). */
  readonly step?: number;
  readonly signature?: string;
  readonly provingMs?: number;
  readonly error?: string;
}

type Listener = (progress: TxProgress) => void;

const WALLET_SIGNING_TIMEOUT_MS = 60_000;

async function awaitWalletSignature(
  request: Promise<Uint8Array>,
): Promise<Uint8Array> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      request,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => {
          reject(
            new Error(
              "The wallet did not respond within 60 seconds. This transaction was not broadcast. Close its pending request, check that Backpack is using Devnet, then try again.",
            ),
          );
        }, WALLET_SIGNING_TIMEOUT_MS);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Observes one SDK action at a time. The wrapped signer and RPC report signature,
 * broadcast, and confirmation; `proofOptions` reports proving.
 */
export class TxTracker {
  #listener: Listener | null = null;
  #signatures = 0;
  #provingStarted: number | null = null;
  #provingMs: number | undefined;

  /** True while an action is running in this tab. */
  get busy(): boolean {
    return this.#listener !== null;
  }

  async run<T>(
    listener: Listener,
    action: (proofOptions: ProofExecutionOptions) => Promise<T>,
  ): Promise<T> {
    if (this.#listener !== null)
      throw new Error("another transaction is in progress");
    this.#listener = listener;
    this.#signatures = 0;
    this.#provingStarted = null;
    this.#provingMs = undefined;
    try {
      this.#emit({ phase: "building" });
      const result = await action({
        onProgress: (progress) => {
          if (progress.phase === "proving" && this.#provingStarted === null) {
            this.#provingStarted = performance.now();
          }
          this.#emit({ phase: "proving" });
        },
      });
      this.#emit({ phase: "confirmed", provingMs: this.#provingMs });
      return result;
    } catch (error) {
      this.#emit({ phase: "failed", error: describeError(error) });
      throw error;
    } finally {
      this.#listener = null;
    }
  }

  signer(inner: BeatExSigner): BeatExSigner {
    return {
      address: inner.address,
      signTransaction: async (transaction) => {
        this.#closeProving();
        this.#signatures += 1;
        this.#emit({ phase: "awaiting-signature", step: this.#signatures });
        return awaitWalletSignature(inner.signTransaction(transaction));
      },
    };
  }

  rpc(inner: BeatExRpc): BeatExRpc {
    return {
      getAccount: (...args) => inner.getAccount(...args),
      getMultipleAccounts: (...args) => inner.getMultipleAccounts(...args),
      getSlot: (...args) => inner.getSlot(...args),
      getLatestBlockhash: (...args) => inner.getLatestBlockhash(...args),
      simulateTransaction: (...args) => {
        this.#emit({ phase: "simulating", step: this.#signatures });
        return inner.simulateTransaction(...args);
      },
      sendRawTransaction: async (...args) => {
        const signature = await inner.sendRawTransaction(...args);
        this.#emit({ phase: "submitted", step: this.#signatures, signature });
        return signature;
      },
      confirmTransaction: (...args) => inner.confirmTransaction(...args),
    };
  }

  #closeProving() {
    if (this.#provingStarted !== null && this.#provingMs === undefined) {
      this.#provingMs = performance.now() - this.#provingStarted;
    }
  }

  #emit(progress: TxProgress) {
    this.#listener?.(progress);
  }
}

export function describeError(error: unknown): string {
  if (error instanceof Error) {
    // submit_queued_order's deadline check: the auction stopped collecting
    // orders before this one could be checked. It failed before signing.
    if (/QueueInvariant/.test(error.message)) {
      return "This auction closed before your order arrived. Nothing was sent; place it again in the next auction.";
    }
    if (/simulat|verif|blocked|blockhash/i.test(error.message)) {
      return error.message;
    }
    if (/reject|declin|denied/i.test(error.message)) {
      return "You declined in your wallet. Nothing was sent.";
    }
    return error.message;
  }
  return "Something went wrong.";
}
