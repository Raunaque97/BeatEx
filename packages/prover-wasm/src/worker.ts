/// <reference lib="webworker" />

import { createTraderWasmRuntime, type TraderWasmRuntime } from "./runtime.js";
import initWasm, {
  TraderProver,
  traderProverApiVersion,
} from "./generated/beatex_trader_wasm_prover.js";
import type {
  MainToWorkerMessage,
  TraderCircuit,
  TraderProofOperation,
  WorkerToMainMessage,
} from "./types.js";

const scope = globalThis as unknown as DedicatedWorkerGlobalScope;
const loaded = new Map<TraderCircuit, { backend: string; circuitVersion: number }>();
const active = new Set<number>();
const cancelled = new Set<number>();
let runtime: TraderWasmRuntime | undefined;

scope.addEventListener("message", (event: MessageEvent<MainToWorkerMessage>) => {
  void handle(event.data);
});

async function handle(message: MainToWorkerMessage): Promise<void> {
  if (message.type === "cancel") {
    if (active.has(message.id)) cancelled.add(message.id);
    return;
  }
  if (message.type === "dispose") {
    loaded.clear();
    runtime?.dispose();
    runtime = undefined;
    scope.close();
    return;
  }

  active.add(message.id);
  try {
    if (message.type === "initialize") {
      if (runtime === undefined) {
        runtime = await createTraderWasmRuntime(message.runtime, async () => ({
          default: initWasm,
          TraderProver,
          traderProverApiVersion,
        }));
      }
      throwIfCancelled(message.id);
      post({ type: "result", id: message.id, result: undefined });
      return;
    }
    if (runtime === undefined) throw new Error("Trader prover runtime is not initialized");
    if (message.type === "load") {
      runtime.loadProvingKey(message.circuit, message.provingKey);
      throwIfCancelled(message.id);
      loaded.set(message.circuit, {
        backend: message.backend,
        circuitVersion: message.circuitVersion,
      });
      post({ type: "result", id: message.id, result: undefined });
      return;
    }

    throwIfCancelled(message.id);
    for (const circuit of requiredCircuits(message.operation)) {
      if (!loaded.has(circuit)) throw new Error(`${circuit} proving key is not loaded`);
    }
    const metadata = loaded.get(message.circuit);
    if (metadata === undefined) throw new Error(`${message.circuit} proving key is not loaded`);
    post({ type: "progress", id: message.id, progress: { phase: "witness" } });
    const started = performance.now();
    post({ type: "progress", id: message.id, progress: { phase: "proving" } });
    const result = runtime.prove(message.operation, message.request);
    throwIfCancelled(message.id);
    post({ type: "progress", id: message.id, progress: { phase: "serializing" } });
    post({
      type: "result",
      id: message.id,
      result: {
        ...(isObject(result) ? result : { result }),
        backend: metadata.backend,
        circuitVersion: metadata.circuitVersion,
        provingTimeMs: performance.now() - started,
      },
    });
  } catch (error) {
    post({ type: "error", id: message.id, error: serializeError(error) });
  } finally {
    active.delete(message.id);
    cancelled.delete(message.id);
  }
}

function requiredCircuits(operation: TraderProofOperation): TraderCircuit[] {
  switch (operation) {
    case "deposit":
    case "withdrawal":
      return ["balance"];
    case "order":
      return ["order"];
    case "release":
      return ["release"];
    case "settlement":
      return ["settlement"];
    case "replacement":
      return ["release", "order"];
    case "rollover":
      return ["settlement", "order"];
    default:
      return [];
  }
}

function throwIfCancelled(id: number): void {
  if (cancelled.has(id)) throw new DOMException("Proof generation was aborted", "AbortError");
}

function post(message: WorkerToMainMessage): void {
  scope.postMessage(message);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function serializeError(error: unknown): { name: string; message: string; stack?: string } {
  if (error instanceof Error) {
    return {
      name: error.name,
      message: error.message,
      ...(error.stack === undefined ? {} : { stack: error.stack }),
    };
  }
  return { name: "Error", message: String(error) };
}
