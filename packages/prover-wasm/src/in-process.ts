import type {
  MainToWorkerMessage,
  ProverWorker,
  ProverWorkerFactory,
  WorkerMessageEventLike,
  WorkerToMainMessage,
} from "./types.js";
import { createWorkerHandler } from "./worker-core.js";

type MessageListener = (event: WorkerMessageEventLike) => void;

/**
 * Runs the prover on the calling thread, for servers with no Web Worker. Proving
 * blocks that thread, which suits a function handling one job at a time.
 */
export const createInProcessProverWorker: ProverWorkerFactory = (): ProverWorker => {
  const listeners = new Set<MessageListener>();
  const emit = (message: WorkerToMainMessage | { readonly type: "ready" }): void => {
    // Reply asynchronously, as a real worker does.
    queueMicrotask(() => {
      for (const listener of listeners) listener({ data: message });
    });
  };
  const handle = createWorkerHandler(emit, () => listeners.clear());
  // The caller attaches its listeners after this factory returns.
  setTimeout(() => emit({ type: "ready" }), 0);
  return {
    postMessage: (message) => void handle(message as MainToWorkerMessage),
    addEventListener: (type: string, listener: unknown) => {
      if (type === "message") listeners.add(listener as MessageListener);
    },
    removeEventListener: (type: string, listener: unknown) => {
      if (type === "message") listeners.delete(listener as MessageListener);
    },
    terminate: () => listeners.clear(),
  };
};
