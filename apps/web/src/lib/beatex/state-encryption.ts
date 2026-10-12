import type { WebStorageLike } from "@beatex/sdk";

/**
 * Private BeatEx state (balance openings, order secrets) is encrypted at rest with
 * AES-256-GCM. The key is derived with HKDF from the wallet's Ed25519 signature over a
 * fixed message. Ed25519 signatures are deterministic, so the same wallet re-derives the
 * same key on any device, and an exported backup only opens with that wallet. The key is
 * non-extractable and held in memory for the session only.
 */

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export interface StateKeyScope {
  readonly cluster: string;
  readonly programId: string;
  readonly owner: string;
}

/** The exact bytes the wallet signs to unlock private state. Changing them changes the key. */
export function stateKeyMessage(scope: StateKeyScope): Uint8Array {
  return encoder.encode(
    [
      "BeatEx: unlock private trading state",
      "",
      "Signing this message decrypts your BeatEx balance and order secrets in this browser.",
      "It does not authorize a transaction or move funds.",
      "",
      `Cluster: ${scope.cluster}`,
      `Program: ${scope.programId}`,
      `Wallet: ${scope.owner}`,
      "Version: 1",
    ].join("\n"),
  );
}

export async function deriveStateKey(
  signature: Uint8Array,
  scope: StateKeyScope,
): Promise<CryptoKey> {
  if (signature.length !== 64) {
    throw new Error("expected a 64-byte Ed25519 signature");
  }
  const material = await crypto.subtle.importKey(
    "raw",
    signature as BufferSource,
    "HKDF",
    false,
    ["deriveKey"],
  );
  return crypto.subtle.deriveKey(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt: encoder.encode("beatex/private-state/v1"),
      info: encoder.encode(
        `${scope.cluster}|${scope.programId}|${scope.owner}`,
      ),
    },
    material,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
}

/** Minimal async key/value store; IndexedDB in the browser, a Map in tests. */
export interface RawStore {
  get(key: string): Promise<unknown>;
  put(key: string, value: unknown): Promise<void>;
}

interface SealedValue {
  readonly v: 1;
  readonly iv: Uint8Array;
  readonly data: Uint8Array;
}

export class StateDecryptionError extends Error {
  constructor() {
    super(
      "This browser's BeatEx state was encrypted by a different wallet signature and cannot be opened.",
    );
    this.name = "StateDecryptionError";
  }
}

/**
 * A `WebStorageLike` whose values are sealed with AES-GCM. The storage key is bound as
 * additional authenticated data, so a ciphertext cannot be replayed under another key.
 */
export class EncryptedStateStore implements WebStorageLike {
  constructor(
    private readonly key: CryptoKey,
    private readonly store: RawStore,
  ) {}

  async getItem(name: string): Promise<string | null> {
    const sealed = (await this.store.get(name)) as SealedValue | undefined;
    if (sealed === undefined || sealed === null) return null;
    if (sealed.v !== 1)
      throw new Error("unsupported private-state envelope version");
    try {
      const plain = await crypto.subtle.decrypt(
        {
          name: "AES-GCM",
          iv: sealed.iv as BufferSource,
          additionalData: encoder.encode(name),
        },
        this.key,
        sealed.data as BufferSource,
      );
      return decoder.decode(plain);
    } catch {
      throw new StateDecryptionError();
    }
  }

  async setItem(name: string, value: string): Promise<void> {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const data = new Uint8Array(
      await crypto.subtle.encrypt(
        { name: "AES-GCM", iv, additionalData: encoder.encode(name) },
        this.key,
        encoder.encode(value),
      ),
    );
    const sealed: SealedValue = { v: 1, iv, data };
    await this.store.put(name, sealed);
  }
}

const DB_NAME = "beatex";
const STORE_NAME = "private-state";

/** IndexedDB-backed `RawStore`; values keep their `Uint8Array` types via structured clone. */
export function indexedDbStore(): RawStore {
  let database: Promise<IDBDatabase> | undefined;
  const open = () =>
    (database ??= new Promise((resolve, reject) => {
      const request = indexedDB.open(DB_NAME, 1);
      request.onupgradeneeded = () =>
        request.result.createObjectStore(STORE_NAME);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    }));
  const run = async <T>(
    mode: IDBTransactionMode,
    operation: (store: IDBObjectStore) => IDBRequest<T>,
  ): Promise<T> => {
    const db = await open();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(STORE_NAME, mode);
      const request = operation(transaction.objectStore(STORE_NAME));
      transaction.oncomplete = () => resolve(request.result);
      transaction.onerror = () => reject(transaction.error);
      transaction.onabort = () => reject(transaction.error);
    });
  };
  return {
    get: (key) => run("readonly", (store) => store.get(key)),
    put: async (key, value) => {
      await run("readwrite", (store) => store.put(value, key));
    },
  };
}
