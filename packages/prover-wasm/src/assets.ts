import type { ProofCancellationSignal } from "@beatex/sdk";
import { ProverAssetIntegrityError } from "./errors.js";
import type {
  ProverAssetCache,
  ProverAssetDescriptor,
  ProverAssetTransport,
} from "./types.js";

const HEX_SHA256 = /^[0-9a-f]{64}$/;

export class FetchAssetTransport implements ProverAssetTransport {
  async load(url: string, signal?: ProofCancellationSignal): Promise<ArrayBuffer> {
    const response = await fetch(url, signal === undefined ? undefined : { signal: signal as AbortSignal });
    if (!response.ok) {
      throw new Error(`Unable to fetch prover asset ${url}: HTTP ${response.status}`);
    }
    return response.arrayBuffer();
  }
}

/** Cache Storage adapter. Supplying another implementation is useful for IndexedDB. */
export class CacheStorageAssetCache implements ProverAssetCache {
  readonly #cacheName: string;

  constructor(cacheName = "beatex-prover-assets-v1") {
    this.#cacheName = cacheName;
  }

  async get(key: string): Promise<ArrayBuffer | undefined> {
    const cache = await caches.open(this.#cacheName);
    const response = await cache.match(new Request(key));
    return response?.arrayBuffer();
  }

  async put(key: string, value: ArrayBuffer): Promise<void> {
    const cache = await caches.open(this.#cacheName);
    await cache.put(new Request(key), new Response(value));
  }

  async delete(key: string): Promise<void> {
    const cache = await caches.open(this.#cacheName);
    await cache.delete(new Request(key));
  }
}

export class MemoryAssetCache implements ProverAssetCache {
  readonly #entries = new Map<string, ArrayBuffer>();

  async get(key: string): Promise<ArrayBuffer | undefined> {
    return this.#entries.get(key)?.slice(0);
  }

  async put(key: string, value: ArrayBuffer): Promise<void> {
    this.#entries.set(key, value.slice(0));
  }

  async delete(key: string): Promise<void> {
    this.#entries.delete(key);
  }
}

export function validateAssetDescriptor(descriptor: ProverAssetDescriptor, label: string): void {
  if (!Number.isSafeInteger(descriptor.byteLength) || descriptor.byteLength < 0) {
    throw new TypeError(`${label}.byteLength must be a non-negative safe integer`);
  }
  if (!HEX_SHA256.test(descriptor.sha256)) {
    throw new TypeError(`${label}.sha256 must be a lowercase, 64-character hex digest`);
  }
  if (descriptor.url.length === 0) {
    throw new TypeError(`${label}.url must not be empty`);
  }
}

export function resolveAssetUrl(url: string, base?: string | URL): string {
  if (base === undefined) return url;
  return new URL(url, base).href;
}

export function assetCacheKey(url: string, descriptor: ProverAssetDescriptor): string {
  const parsed = new URL(url, "https://beatex.invalid/");
  parsed.searchParams.set("beatex-sha256", descriptor.sha256);
  return parsed.href;
}

export async function verifyAsset(
  bytes: ArrayBuffer,
  descriptor: ProverAssetDescriptor,
  url: string,
): Promise<void> {
  if (bytes.byteLength !== descriptor.byteLength) {
    throw new ProverAssetIntegrityError(
      url,
      `expected ${descriptor.byteLength} bytes, received ${bytes.byteLength}`,
    );
  }
  if (globalThis.crypto?.subtle === undefined) {
    throw new Error("Web Crypto SHA-256 is required to verify prover assets");
  }
  const digest = await globalThis.crypto.subtle.digest("SHA-256", bytes);
  const actual = [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  if (actual !== descriptor.sha256) {
    throw new ProverAssetIntegrityError(url, `expected SHA-256 ${descriptor.sha256}, received ${actual}`);
  }
}

export class ProverAssetStore {
  readonly #transport: ProverAssetTransport;
  readonly #cache: ProverAssetCache | undefined;
  readonly #base: string | URL | undefined;
  readonly #loaded = new Map<string, Promise<ArrayBuffer>>();

  constructor(options: {
    transport: ProverAssetTransport;
    cache?: ProverAssetCache;
    base?: string | URL;
  }) {
    this.#transport = options.transport;
    this.#cache = options.cache;
    this.#base = options.base;
  }

  load(descriptor: ProverAssetDescriptor, signal?: ProofCancellationSignal): Promise<ArrayBuffer> {
    const url = resolveAssetUrl(descriptor.url, this.#base);
    const key = assetCacheKey(url, descriptor);
    const existing = this.#loaded.get(key);
    if (existing !== undefined) return existing.then((bytes) => bytes.slice(0));

    const pending = this.#loadAndVerify(url, key, descriptor, signal);
    this.#loaded.set(key, pending);
    void pending.then(
      () => {
        if (this.#loaded.get(key) === pending) this.#loaded.delete(key);
      },
      () => {
        if (this.#loaded.get(key) === pending) this.#loaded.delete(key);
      },
    );
    return pending.then((bytes) => bytes.slice(0));
  }

  async #loadAndVerify(
    url: string,
    key: string,
    descriptor: ProverAssetDescriptor,
    signal?: ProofCancellationSignal,
  ): Promise<ArrayBuffer> {
    const cached = await this.#cache?.get(key);
    if (cached !== undefined) {
      try {
        await verifyAsset(cached, descriptor, url);
        return cached;
      } catch (error) {
        await this.#cache?.delete?.(key);
        if (!(error instanceof ProverAssetIntegrityError)) throw error;
      }
    }

    const loaded = await this.#transport.load(url, signal);
    await verifyAsset(loaded, descriptor, url);
    await this.#cache?.put(key, loaded);
    return loaded;
  }
}
