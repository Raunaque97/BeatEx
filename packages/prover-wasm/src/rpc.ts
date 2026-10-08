import type { ProofExecutionOptions, ProvingProgress } from "@beatex/sdk";
import { abortError, ProverWorkerError } from "./errors.js";
import type {
  MainToWorkerMessage,
  ProverWorker,
  WorkerErrorEventLike,
  WorkerMessageEventLike,
  WorkerToMainMessage,
} from "./types.js";

interface PendingRequest {
  readonly resolve: (value: unknown) => void;
  readonly reject: (reason: unknown) => void;
  readonly onProgress?: (progress: ProvingProgress) => void;
  readonly removeAbort?: () => void;
}

export class WorkerRpc {
  readonly #worker: ProverWorker;
  readonly #pending = new Map<number, PendingRequest>();
  #nextId = 1;
  #disposed = false;

  constructor(worker: ProverWorker) {
    this.#worker = worker;
    worker.addEventListener("message", this.#onMessage);
    worker.addEventListener("error", this.#onWorkerError);
  }

  request<T>(
    createMessage: (id: number) => MainToWorkerMessage,
    options?: ProofExecutionOptions,
    transfer?: Transferable[],
  ): Promise<T> {
    if (this.#disposed) return Promise.reject(new ProverWorkerError("The prover has been disposed"));
    if (options?.signal?.aborted === true) return Promise.reject(abortError(options.signal.reason));

    const id = this.#nextId++;
    return new Promise<T>((resolve, reject) => {
      const onAbort = (): void => {
        this.#worker.postMessage({ type: "cancel", id } satisfies MainToWorkerMessage);
        this.#finish(id)?.reject(abortError(options?.signal?.reason));
      };
      const removeAbort = options?.signal?.addEventListener === undefined
        ? undefined
        : (): void => options.signal?.removeEventListener?.("abort", onAbort);
      const pending: PendingRequest = {
        resolve: resolve as (value: unknown) => void,
        reject,
        ...(options?.onProgress === undefined ? {} : { onProgress: options.onProgress }),
        ...(removeAbort === undefined ? {} : { removeAbort }),
      };
      this.#pending.set(id, pending);
      options?.signal?.addEventListener?.("abort", onAbort, { once: true });

      try {
        this.#worker.postMessage(createMessage(id), transfer);
      } catch (error) {
        this.#finish(id)?.reject(error);
      }
    });
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#worker.postMessage({ type: "dispose" } satisfies MainToWorkerMessage);
    this.#worker.terminate();
    this.#worker.removeEventListener("message", this.#onMessage);
    this.#worker.removeEventListener("error", this.#onWorkerError);
    const error = new ProverWorkerError("The prover was disposed before the request completed");
    for (const id of [...this.#pending.keys()]) this.#finish(id)?.reject(error);
  }

  readonly #onMessage = (event: WorkerMessageEventLike): void => {
    if (!isWorkerResponse(event.data)) return;
    const message = event.data;
    const pending = this.#pending.get(message.id);
    if (pending === undefined) return;

    if (message.type === "progress") {
      pending.onProgress?.(message.progress);
      return;
    }

    this.#finish(message.id);
    if (message.type === "result") {
      pending.resolve(message.result);
    } else {
      const error = new ProverWorkerError(message.error.message);
      error.name = message.error.name;
      if (message.error.stack !== undefined) error.stack = message.error.stack;
      pending.reject(error);
    }
  };

  readonly #onWorkerError = (event: WorkerErrorEventLike): void => {
    const error = new ProverWorkerError(event.message ?? "The prover worker crashed", {
      ...(event.error === undefined ? {} : { cause: event.error }),
    });
    for (const id of [...this.#pending.keys()]) this.#finish(id)?.reject(error);
  };

  #finish(id: number): PendingRequest | undefined {
    const pending = this.#pending.get(id);
    if (pending === undefined) return undefined;
    this.#pending.delete(id);
    pending.removeAbort?.();
    return pending;
  }
}

function isWorkerResponse(value: unknown): value is WorkerToMainMessage {
  if (typeof value !== "object" || value === null || !("type" in value) || !("id" in value)) {
    return false;
  }
  return value.type === "result" || value.type === "error" || value.type === "progress";
}
