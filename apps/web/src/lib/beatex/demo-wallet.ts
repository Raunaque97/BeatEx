import {
  createSignerFromKeyPair,
  generateKeyPair,
  type KeyPairSigner,
} from "@solana/kit";

import { publicEnv } from "@/lib/config/public-env";

/**
 * The Devnet demo wallet: an Ed25519 key pair generated in this browser and kept
 * in IndexedDB. The private key is non-extractable, so page code can sign with it
 * but can never read or export its bytes. There is deliberately no import path,
 * so nobody pastes a real wallet's key into a demo site.
 */

const DB_NAME = "beatex-demo-wallet";
const STORE = "keys";
/** One wallet per cluster; a Devnet key is never offered on another cluster. */
const KEY = `wallet:${publicEnv.cluster}`;

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function isKeyPair(value: unknown): value is CryptoKeyPair {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as CryptoKeyPair).privateKey instanceof CryptoKey &&
    (value as CryptoKeyPair).publicKey instanceof CryptoKey
  );
}

/**
 * Store `candidate` unless a wallet already exists, in one transaction, so two tabs
 * opening at once settle on the same key. Returns whichever key pair is stored.
 */
function keepFirst(
  db: IDBDatabase,
  candidate: CryptoKeyPair,
): Promise<CryptoKeyPair> {
  return new Promise((resolve, reject) => {
    const transaction = db.transaction(STORE, "readwrite");
    const store = transaction.objectStore(STORE);
    let result = candidate;
    const existing = store.get(KEY);
    existing.onsuccess = () => {
      if (isKeyPair(existing.result)) result = existing.result;
      else store.put(candidate, KEY);
    };
    transaction.oncomplete = () => resolve(result);
    transaction.onerror = () => reject(transaction.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

/** This browser's demo wallet, created on first use. */
export async function loadDemoWallet(): Promise<KeyPairSigner> {
  const db = await openDb();
  try {
    // Generated before the transaction because IndexedDB commits on any await.
    const keyPair = await keepFirst(db, await generateKeyPair());
    return await createSignerFromKeyPair(keyPair);
  } finally {
    db.close();
  }
}

/** Forget this browser's demo wallet. Its Devnet funds become unreachable. */
export async function deleteDemoWallet(): Promise<void> {
  const db = await openDb();
  try {
    await new Promise<void>((resolve, reject) => {
      const transaction = db.transaction(STORE, "readwrite");
      transaction.objectStore(STORE).delete(KEY);
      transaction.oncomplete = () => resolve();
      transaction.onerror = () => reject(transaction.error);
    });
  } finally {
    db.close();
  }
}
