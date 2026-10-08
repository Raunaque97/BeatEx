import type {
  BeatExProver,
  CancelOrderProofRequest,
  CancelOrderProofResult,
  DepositProofRequest,
  DepositProofResult,
  OrderProofRequest,
  OrderProofResult,
  ProofExecutionOptions,
  ReplaceOrderProofRequest,
  ReplaceOrderProofResult,
  RolloverProofRequest,
  RolloverProofResult,
  SettlementProofRequest,
  SettlementProofResult,
  WithdrawProofRequest,
  WithdrawProofResult,
} from "@beatex/sdk";
import { FetchAssetTransport, ProverAssetStore, validateAssetDescriptor } from "./assets.js";
import { abortError } from "./errors.js";
import { WorkerRpc } from "./rpc.js";
import type {
  ProverAssetManifest,
  ProverWorker,
  TraderCircuit,
  TraderProofOperation,
  WasmBeatExProverOptions,
} from "./types.js";

/** Web Worker backed implementation of every trader-facing SDK proof method. */
export class WasmBeatExProver implements BeatExProver {
  readonly #manifest: ProverAssetManifest;
  readonly #assets: ProverAssetStore;
  readonly #rpc: WorkerRpc;
  readonly #circuitReady = new Map<TraderCircuit, Promise<void>>();
  #runtimeReady: Promise<void> | undefined;

  constructor(options: WasmBeatExProverOptions) {
    validateManifest(options.manifest);
    this.#manifest = options.manifest;
    this.#assets = new ProverAssetStore({
      transport: options.assetTransport ?? new FetchAssetTransport(),
      ...(options.assetCache === undefined ? {} : { cache: options.assetCache }),
      ...(options.assetBaseUrl === undefined ? {} : { base: options.assetBaseUrl }),
    });
    const factory = options.workerFactory ?? defaultWorkerFactory(options.workerUrl);
    this.#rpc = new WorkerRpc(factory());
  }

  async proveDeposit(
    request: DepositProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<DepositProofResult> {
    return this.#prove("balance", "deposit", request, options);
  }

  async proveOrder(
    request: OrderProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<OrderProofResult> {
    // Ciphertexts are returned untouched: buy[i], sell[i], buy[i + 1], sell[i + 1], ...
    return this.#prove("order", "order", request, options);
  }

  async proveReplacement(
    request: ReplaceOrderProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<ReplaceOrderProofResult> {
    return this.#proveComposite(
      ["release", "order"],
      "release",
      "replacement",
      request,
      options,
    );
  }

  async proveCancellation(
    request: CancelOrderProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<CancelOrderProofResult> {
    return this.#prove("release", "release", request, options);
  }

  async proveSettlement(
    request: SettlementProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<SettlementProofResult> {
    return this.#prove("settlement", "settlement", request, options);
  }

  async proveRollover(
    request: RolloverProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<RolloverProofResult> {
    return this.#proveComposite(
      ["settlement", "order"],
      "settlement",
      "rollover",
      request,
      options,
    );
  }

  async proveWithdrawal(
    request: WithdrawProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<WithdrawProofResult> {
    return this.#prove("balance", "withdrawal", request, options);
  }

  dispose(): void {
    this.#rpc.dispose();
  }

  async #prove<Request, Result>(
    circuit: TraderCircuit,
    operation: TraderProofOperation,
    request: Request,
    options?: ProofExecutionOptions,
  ): Promise<Result> {
    if (isAborted(options)) throw abortError(options?.signal?.reason);
    options?.onProgress?.({ phase: "loading", completed: 0, total: 1 });
    await this.#ensureCircuit(circuit, options);
    if (isAborted(options)) throw abortError(options?.signal?.reason);
    options?.onProgress?.({ phase: "loading", completed: 1, total: 1 });

    const result = await this.#rpc.request<Result>(
      (id) => ({ type: "prove", id, circuit, operation, request }),
      options,
    );
    return result;
  }

  async #proveComposite<Request, Result>(
    circuits: readonly TraderCircuit[],
    primaryCircuit: TraderCircuit,
    operation: TraderProofOperation,
    request: Request,
    options?: ProofExecutionOptions,
  ): Promise<Result> {
    if (isAborted(options)) throw abortError(options?.signal?.reason);
    options?.onProgress?.({ phase: "loading", completed: 0, total: circuits.length });
    for (let index = 0; index < circuits.length; index += 1) {
      await this.#ensureCircuit(circuits[index]!, options);
      options?.onProgress?.({ phase: "loading", completed: index + 1, total: circuits.length });
    }
    if (isAborted(options)) throw abortError(options?.signal?.reason);
    return this.#rpc.request<Result>(
      (id) => ({ type: "prove", id, circuit: primaryCircuit, operation, request }),
      options,
    );
  }

  #ensureRuntime(options?: ProofExecutionOptions): Promise<void> {
    if (this.#runtimeReady !== undefined) return this.#runtimeReady;
    const pending = (async (): Promise<void> => {
      const runtime = await this.#assets.load(this.#manifest.runtime, options?.signal);
      await this.#rpc.request<void>(
        (id) => ({ type: "initialize", id, runtime }),
        options,
        [runtime],
      );
    })();
    this.#runtimeReady = pending;
    pending.catch(() => {
      if (this.#runtimeReady === pending) this.#runtimeReady = undefined;
    });
    return pending;
  }

  #ensureCircuit(circuit: TraderCircuit, options?: ProofExecutionOptions): Promise<void> {
    const existing = this.#circuitReady.get(circuit);
    if (existing !== undefined) return existing;
    const descriptor = this.#manifest.circuits[circuit];
    const pending = (async (): Promise<void> => {
      const [, provingKey] = await Promise.all([
        this.#ensureRuntime(options),
        this.#assets.load(descriptor.provingKey, options?.signal),
      ]);
      await this.#rpc.request<void>(
        (id) => ({
          type: "load",
          id,
          circuit,
          backend: this.#manifest.backend,
          circuitVersion: descriptor.circuitVersion,
          provingKey,
        }),
        options,
        [provingKey],
      );
    })();
    this.#circuitReady.set(circuit, pending);
    pending.catch(() => {
      if (this.#circuitReady.get(circuit) === pending) this.#circuitReady.delete(circuit);
    });
    return pending;
  }
}

export function createWasmBeatExProver(options: WasmBeatExProverOptions): WasmBeatExProver {
  return new WasmBeatExProver(options);
}

function defaultWorkerFactory(workerUrl?: string | URL): () => ProverWorker {
  return () => {
    if (typeof Worker === "undefined") {
      throw new Error("A Worker implementation is required outside a browser");
    }
    return new Worker(workerUrl ?? new URL("./worker.js", import.meta.url), { type: "module" }) as unknown as ProverWorker;
  };
}

function validateManifest(manifest: ProverAssetManifest): void {
  if (manifest.schemaVersion !== 1) throw new TypeError("Unsupported prover manifest schema version");
  if (manifest.backend !== "beatex-arkworks-groth16-wasm-v1") {
    throw new TypeError(`Unsupported prover backend ${manifest.backend}`);
  }
  if (manifest.provingKeyEncoding !== "arkworks-canonical-uncompressed-v1") {
    throw new TypeError(`Unsupported proving-key encoding ${manifest.provingKeyEncoding}`);
  }
  validateAssetDescriptor(manifest.runtime, "manifest.runtime");
  for (const circuit of ["balance", "order", "release", "settlement"] as const) {
    const descriptor = manifest.circuits[circuit];
    if (descriptor.circuitVersion !== 1) {
      throw new TypeError(`manifest.circuits.${circuit}.circuitVersion must be 1`);
    }
    validateAssetDescriptor(descriptor.provingKey, `manifest.circuits.${circuit}.provingKey`);
  }
}

function isAborted(options?: ProofExecutionOptions): boolean {
  return options?.signal?.aborted === true;
}
