import assert from "node:assert/strict";
import test from "node:test";
import { BrowserPrivateStateStorage } from "../dist/index.js";

const key = { cluster: "devnet", programId: "program", market: "market", owner: "owner" };
const state = {
  balanceRevision: 1n,
  keyEpoch: "epoch",
  balance: {
    baseValue: 5n,
    quoteValue: 7n,
    baseRandomness: new Uint8Array(32).fill(1),
    quoteRandomness: new Uint8Array(32).fill(2),
  },
  encryptedBase: new Uint8Array(64),
  encryptedQuote: new Uint8Array(64),
  activeOrder: null,
};

test("browser storage runs the journal over an asynchronous backend", async () => {
  const entries = new Map();
  const asyncBackend = {
    getItem: async (name) => entries.get(name) ?? null,
    setItem: async (name, value) => {
      await new Promise((resolve) => setTimeout(resolve, 1));
      entries.set(name, value);
    },
  };
  const storage = new BrowserPrivateStateStorage(asyncBackend, { allowSingleContextFallback: true });

  assert.deepEqual(await storage.load(key), { record: null, pending: null });
  await storage.stage(key, { id: "t1", operation: "deposit", expectedStorageRevision: null, nextState: state });
  assert.equal((await storage.load(key)).pending.id, "t1");
  await storage.markBroadcast(key, "t1", "sig", "hash", 10n);
  const record = await storage.commitConfirmed(key, "t1", { signature: "sig", slot: 1n });
  assert.equal(record.storageRevision, 0n);
  const loaded = await storage.load(key);
  assert.equal(loaded.pending, null);
  assert.equal(loaded.record.state.balance.quoteValue, 7n);
  assert.deepEqual(loaded.record.state.balance.baseRandomness, new Uint8Array(32).fill(1));
});
