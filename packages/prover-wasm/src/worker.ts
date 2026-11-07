/// <reference lib="webworker" />

import type { MainToWorkerMessage } from "./types.js";
import { createWorkerHandler } from "./worker-core.js";

const scope = globalThis as unknown as DedicatedWorkerGlobalScope;
const handle = createWorkerHandler(
  (message) => scope.postMessage(message),
  () => scope.close(),
);

scope.addEventListener("message", (event: MessageEvent<MainToWorkerMessage>) => {
  void handle(event.data);
});
// Announce readiness only once the listener exists; the main thread queues until then.
scope.postMessage({ type: "ready" });
