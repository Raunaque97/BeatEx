import assert from "node:assert/strict";
import test from "node:test";
import {
  BEATEX_PROGRAM_ADDRESS,
  InMemoryPrivateStateStorage,
  TransactionOutcomeError,
  createBeatExClient,
  createBeatExRpc,
} from "../dist/index.js";

const MARKET = "3haCd5iqRJuk9FGVFRdkW6jMnKQL4CH4vgwdpVc8XzxS";
const OWNER = "Db3M2ef9iDH1rVeFhmQzoDnywbu4wdQWDonqGfs2ATQS";
const KEY = { cluster: "devnet", programId: BEATEX_PROGRAM_ADDRESS, market: MARKET, owner: OWNER };
const STATE = {
  balanceRevision: 0n,
  keyEpoch: "AoJ755HzDcFx8VZB3rpotAWRszxv7dCVZYX9WSRe3abe",
  balance: {
    baseValue: 0n,
    quoteValue: 0n,
    baseRandomness: new Uint8Array(32),
    quoteRandomness: new Uint8Array(32),
  },
  encryptedBase: new Uint8Array(64),
  encryptedQuote: new Uint8Array(64),
  activeOrder: null,
};

/** A fetch that answers JSON-RPC calls from `handlers`, recording each method. */
function fakeFetch(handlers) {
  const calls = [];
  const fetch = async (_url, options) => {
    const { method, params } = JSON.parse(options.body);
    calls.push(method);
    const reply = await handlers[method](params, calls.filter((name) => name === method).length);
    if (reply instanceof Response) return reply;
    return new Response(JSON.stringify({ jsonrpc: "2.0", id: 1, result: reply }));
  };
  return { fetch, calls };
}

const fast = { confirmationPollMs: 1, retryBaseMs: 1 };

test("RPC calls retry rate limits and outages, then give up", async () => {
  const { fetch, calls } = fakeFetch({
    simulateTransaction: (_params, attempt) =>
      attempt === 1
        ? new Response("slow down", { status: 429 })
        : attempt === 2
          ? new Response("unavailable", { status: 503 })
          : { value: { err: null, logs: [] } },
  });
  const rpc = createBeatExRpc("https://rpc.example", { fetch, ...fast });
  assert.equal((await rpc.simulateTransaction(Uint8Array.of(1))).err, null);
  assert.equal(calls.length, 3);

  const limited = fakeFetch({
    simulateTransaction: () => new Response("slow down", { status: 429 }),
  });
  await assert.rejects(
    createBeatExRpc("https://rpc.example", { fetch: limited.fetch, ...fast, retries: 2 }).simulateTransaction(
      Uint8Array.of(1),
    ),
    /HTTP 429/,
  );
  assert.equal(limited.calls.length, 3);
});

test("confirmation waits through transient errors and reports confirmed", async () => {
  const { fetch } = fakeFetch({
    getSignatureStatuses: (_params, attempt) =>
      attempt <= 4
        ? new Response("rate limited", { status: 429 })
        : { value: [{ err: null, slot: 7, confirmationStatus: "confirmed" }] },
  });
  const rpc = createBeatExRpc("https://rpc.example", { fetch, ...fast, retries: 1 });
  const confirmed = await rpc.confirmTransaction("sig", { lastValidBlockHeight: 100n });
  assert.equal(confirmed.slot, 7n);
});

test("confirmation reports failed and expired transactions as definite outcomes", async () => {
  const failed = fakeFetch({
    getSignatureStatuses: () => ({
      value: [{ err: { InstructionError: [0, { Custom: 1 }] }, slot: 7, confirmationStatus: "confirmed" }],
    }),
  });
  await assert.rejects(
    createBeatExRpc("https://rpc.example", { fetch: failed.fetch, ...fast }).confirmTransaction("sig"),
    (error) => error instanceof TransactionOutcomeError && error.outcome === "failed",
  );

  const expired = fakeFetch({
    getSignatureStatuses: () => ({ value: [null] }),
    getBlockHeight: () => 101,
  });
  await assert.rejects(
    createBeatExRpc("https://rpc.example", { fetch: expired.fetch, ...fast }).confirmTransaction("sig", {
      lastValidBlockHeight: 100n,
    }),
    (error) => error instanceof TransactionOutcomeError && error.outcome === "expired",
  );
  // The expiry is confirmed with a second status lookup before giving up.
  assert.deepEqual(expired.calls, ["getSignatureStatuses", "getBlockHeight", "getSignatureStatuses"]);
});

async function clientWithPending(confirmTransaction) {
  const storage = new InMemoryPrivateStateStorage();
  await storage.stage(KEY, {
    id: "transition-1",
    operation: "initialize",
    expectedStorageRevision: null,
    nextState: STATE,
  });
  await storage.markBroadcast(KEY, "transition-1", "sig", "blockhash", 99n);
  const client = createBeatExClient({
    cluster: "devnet",
    storage,
    prover: {},
    rpc: { confirmTransaction },
    signer: { address: OWNER, signTransaction: async (bytes) => bytes },
  });
  return { client, storage };
}

test("recovery commits a transition whose transaction confirmed", async () => {
  const { client, storage } = await clientWithPending(async () => ({
    signature: "sig",
    slot: 9n,
    confirmationStatus: "confirmed",
  }));
  const record = await client.recoverPendingTransition(MARKET);
  assert.equal(record?.state.balanceRevision, 0n);
  assert.equal((await storage.load(KEY)).pending, null);
});

test("recovery drops a transition whose transaction failed or expired", async () => {
  for (const outcome of ["failed", "expired"]) {
    const { client, storage } = await clientWithPending(async () => {
      throw new TransactionOutcomeError(`transaction ${outcome}`, outcome);
    });
    assert.equal(await client.recoverPendingTransition(MARKET), null);
    const snapshot = await storage.load(KEY);
    assert.equal(snapshot.pending, null);
    assert.equal(snapshot.record, null);
  }
});

test("recovery keeps the transition when the outcome is still unknown", async () => {
  const { client, storage } = await clientWithPending(async () => {
    throw new Error("timed out confirming transaction sig");
  });
  await assert.rejects(client.recoverPendingTransition(MARKET), /timed out/);
  assert.equal((await storage.load(KEY)).pending?.id, "transition-1");
});
