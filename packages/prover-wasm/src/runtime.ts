import type { TraderCircuit, TraderProofOperation } from "./types.js";

interface WasmTraderProver {
  loadProvingKey(circuit: TraderCircuit, bytes: Uint8Array): void;
  isProvingKeyLoaded(circuit: TraderCircuit): boolean;
  proveDeposit(request: unknown): unknown;
  proveWithdrawal(request: unknown): unknown;
  proveOrder(request: unknown): unknown;
  proveCancellation(request: unknown): unknown;
  proveSettlement(request: unknown): unknown;
  proveReplacement(request: unknown): unknown;
  proveRollover(request: unknown): unknown;
  free?(): void;
}

export interface TraderWasmBindings {
  readonly default: (input: {
    module_or_path: ArrayBuffer | Uint8Array | WebAssembly.Module;
  }) => Promise<unknown>;
  readonly TraderProver: new () => WasmTraderProver;
  readonly traderProverApiVersion: () => number;
}

export type TraderWasmModuleLoader = () => Promise<TraderWasmBindings>;

export interface TraderWasmRuntime {
  loadProvingKey(circuit: TraderCircuit, provingKey: ArrayBuffer): void;
  prove(operation: TraderProofOperation, request: unknown): unknown;
  dispose(): void;
}

/** Initialize the package's wasm-bindgen glue with integrity-checked WASM bytes. */
export async function createTraderWasmRuntime(
  bytes: ArrayBuffer,
  loader: TraderWasmModuleLoader = loadGeneratedBindings,
): Promise<TraderWasmRuntime> {
  const bindings = await loader();
  await bindings.default({ module_or_path: bytes });
  const version = bindings.traderProverApiVersion();
  if (version !== 1) throw new TypeError(`Unsupported trader prover WASM API version ${version}`);
  return new BindgenTraderWasmRuntime(new bindings.TraderProver());
}

class BindgenTraderWasmRuntime implements TraderWasmRuntime {
  readonly #prover: WasmTraderProver;

  constructor(prover: WasmTraderProver) {
    this.#prover = prover;
  }

  loadProvingKey(circuit: TraderCircuit, provingKey: ArrayBuffer): void {
    this.#prover.loadProvingKey(circuit, new Uint8Array(provingKey));
    if (!this.#prover.isProvingKeyLoaded(circuit)) {
      throw new Error(`${circuit} proving key did not remain loaded`);
    }
  }

  prove(operation: TraderProofOperation, request: unknown): unknown {
    switch (operation) {
      case "deposit":
        return this.#prover.proveDeposit(request);
      case "withdrawal":
        return this.#prover.proveWithdrawal(request);
      case "order":
        return this.#prover.proveOrder(request);
      case "release":
        return this.#prover.proveCancellation(request);
      case "settlement":
        return this.#prover.proveSettlement(request);
      case "replacement":
        return this.#prover.proveReplacement(request);
      case "rollover":
        return this.#prover.proveRollover(request);
    }
  }

  dispose(): void {
    this.#prover.free?.();
  }
}

async function loadGeneratedBindings(): Promise<TraderWasmBindings> {
  const url = new URL("./generated/beatex_trader_wasm_prover.js", import.meta.url).href;
  return import(/* @vite-ignore */ url) as Promise<TraderWasmBindings>;
}
