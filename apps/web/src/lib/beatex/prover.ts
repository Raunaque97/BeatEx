import {
  CacheStorageAssetCache,
  createWasmBeatExProver,
  type ProverAssetManifest,
  type ProverWorker,
  type WasmBeatExProver,
} from "@beatex/prover-wasm";

import { publicEnv } from "@/lib/config/public-env";

let shared: Promise<WasmBeatExProver> | undefined;

/**
 * One long-lived prover worker per page. Proving keys are fetched on first use,
 * verified against the manifest's SHA-256 digests, and cached in Cache Storage.
 */
export function getBrowserProver(): Promise<WasmBeatExProver> {
  shared ??= (async () => {
    const manifestUrl = new URL(
      publicEnv.proverManifestUrl,
      window.location.href,
    );
    const response = await fetch(manifestUrl, { cache: "no-cache" });
    if (!response.ok)
      throw new Error(`Prover manifest returned HTTP ${response.status}`);
    const manifest = (await response.json()) as ProverAssetManifest;
    return createWasmBeatExProver({
      manifest,
      assetBaseUrl: new URL("./", manifestUrl),
      assetCache:
        typeof caches === "undefined"
          ? undefined
          : new CacheStorageAssetCache(),
      // A classic worker: Turbopack's worker bootstrap loads chunks with importScripts(),
      // which module workers reject.
      workerFactory: () =>
        new Worker(
          new URL("./prover.worker.ts", import.meta.url),
        ) as unknown as ProverWorker,
    });
  })();
  shared.catch(() => {
    shared = undefined;
  });
  return shared;
}
