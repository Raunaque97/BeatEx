import assert from "node:assert/strict";
import test from "node:test";
import {
  MemoryAssetCache,
  ProverAssetStore,
  ProverAssetIntegrityError,
  createTraderWasmRuntime,
  createWasmBeatExProver,
} from "../dist/index.js";

const bytes = (...values) => Uint8Array.from(values).buffer;

async function descriptor(url, value) {
  const digest = await crypto.subtle.digest("SHA-256", value);
  return {
    url,
    byteLength: value.byteLength,
    sha256: Buffer.from(digest).toString("hex"),
  };
}

async function fixture() {
  const assets = new Map([
    ["https://assets.example/runtime.wasm", bytes(0, 97, 115, 109)],
    ["https://assets.example/balance.pk", bytes(1, 1)],
    ["https://assets.example/order.pk", bytes(2, 2, 2)],
    ["https://assets.example/release.pk", bytes(3, 3, 3, 3)],
    ["https://assets.example/settlement.pk", bytes(4, 4, 4, 4, 4)],
  ]);
  const get = (url) => {
    const value = assets.get(url);
    assert.ok(value, `missing fixture ${url}`);
    return value;
  };
  return {
    assets,
    manifest: {
      schemaVersion: 1,
      backend: "beatex-arkworks-groth16-wasm-v1",
      provingKeyEncoding: "arkworks-canonical-uncompressed-v1",
      runtime: await descriptor("runtime.wasm", get("https://assets.example/runtime.wasm")),
      circuits: {
        balance: {
          circuitVersion: 1,
          provingKey: await descriptor("balance.pk", get("https://assets.example/balance.pk")),
        },
        order: {
          circuitVersion: 1,
          provingKey: await descriptor("order.pk", get("https://assets.example/order.pk")),
        },
        release: {
          circuitVersion: 1,
          provingKey: await descriptor("release.pk", get("https://assets.example/release.pk")),
        },
        settlement: {
          circuitVersion: 1,
          provingKey: await descriptor("settlement.pk", get("https://assets.example/settlement.pk")),
        },
      },
    },
  };
}

class FakeTransport {
  constructor(assets) {
    this.assets = assets;
    this.loads = [];
  }

  async load(url) {
    this.loads.push(url);
    const value = this.assets.get(url);
    if (value === undefined) throw new Error(`missing ${url}`);
    return value.slice(0);
  }
}

class FakeWorker {
  messageListeners = new Set();
  errorListeners = new Set();
  messages = [];
  proveDelay = 0;
  terminated = false;

  postMessage(message) {
    this.messages.push(message);
    if (message.type === "dispose" || message.type === "cancel") return;
    const reply = () => {
      if (message.type === "initialize" || message.type === "load") {
        this.emit({ type: "result", id: message.id, result: undefined });
        return;
      }
      const metadata = { backend: "fake", provingTimeMs: 5, circuitVersion: 9 };
      let result;
      if (message.operation === "replacement") {
        result = {
          release: { releasedBalance: { source: "release" }, ...metadata },
          replacement: { observedOldBalance: { source: "release" }, ...metadata },
          ...metadata,
        };
      } else if (message.operation === "rollover") {
        result = {
          settlement: { creditedBalance: { source: "settlement" }, ...metadata },
          nextOrder: { observedOldBalance: { source: "settlement" }, ...metadata },
          ...metadata,
        };
      } else if (message.operation === "release") {
        result = { releasedBalance: { source: "release" }, ...metadata };
      } else if (message.operation === "settlement") {
        result = { creditedBalance: { source: "settlement" }, ...metadata };
      } else if (message.operation === "order") {
        result = {
          observedOldBalance: message.request.oldBalance,
          // Canonical order is interleaved and the bridge must preserve it exactly.
          ciphertexts: [Uint8Array.of(10), Uint8Array.of(20), Uint8Array.of(11), Uint8Array.of(21)],
          ...metadata,
        };
      } else {
        result = { operation: message.operation, ...metadata };
      }
      this.emit({ type: "progress", id: message.id, progress: { phase: "proving" } });
      this.emit({ type: "result", id: message.id, result });
    };
    if (this.proveDelay > 0 && message.type === "prove") setTimeout(reply, this.proveDelay);
    else queueMicrotask(reply);
  }

  addEventListener(type, listener) {
    (type === "message" ? this.messageListeners : this.errorListeners).add(listener);
  }

  removeEventListener(type, listener) {
    (type === "message" ? this.messageListeners : this.errorListeners).delete(listener);
  }

  terminate() {
    this.terminated = true;
  }

  emit(data) {
    for (const listener of this.messageListeners) listener({ data });
  }
}

async function setup(overrides = {}) {
  const { assets, manifest } = await fixture();
  const worker = overrides.worker ?? new FakeWorker();
  const transport = overrides.transport ?? new FakeTransport(assets);
  const prover = createWasmBeatExProver({
    manifest,
    assetBaseUrl: "https://assets.example/",
    assetTransport: transport,
    workerFactory: () => worker,
    ...overrides.options,
  });
  return { prover, worker, transport, manifest, assets };
}

test("one worker/runtime lazily loads only the required circuit keys", async () => {
  const { prover, worker, transport } = await setup();
  const progress = [];
  const deposit = await prover.proveDeposit({ marker: "deposit" }, { onProgress: (value) => progress.push(value.phase) });
  assert.equal(deposit.operation, "deposit");
  assert.deepEqual(transport.loads, [
    "https://assets.example/runtime.wasm",
    "https://assets.example/balance.pk",
  ]);

  const withdrawal = await prover.proveWithdrawal({ marker: "withdrawal" });
  assert.equal(withdrawal.operation, "withdrawal");
  assert.equal(transport.loads.length, 2, "balance assets are reused");

  const order = await prover.proveOrder({ oldBalance: { source: "caller" } });
  assert.deepEqual(order.ciphertexts.map((value) => value[0]), [10, 20, 11, 21]);
  assert.deepEqual(transport.loads, [
    "https://assets.example/runtime.wasm",
    "https://assets.example/balance.pk",
    "https://assets.example/order.pk",
  ]);
  assert.equal(worker.messages.filter((message) => message.type === "initialize").length, 1);
  assert.ok(progress.includes("loading") && progress.includes("proving"));
  prover.dispose();
  assert.equal(worker.terminated, true);
});

test("replacement and rollover compose primitive proofs using mandatory output balances", async () => {
  const { prover } = await setup();
  const replacement = await prover.proveReplacement({
    release: { marker: "release" },
    replacement: { marker: "next" },
  });
  assert.deepEqual(replacement.replacement.observedOldBalance, { source: "release" });
  assert.equal(replacement.provingTimeMs, 5);

  const rollover = await prover.proveRollover({
    settlement: { marker: "settlement" },
    nextOrder: { marker: "next" },
  });
  assert.deepEqual(rollover.nextOrder.observedOldBalance, { source: "settlement" });
  assert.equal(rollover.provingTimeMs, 5);
  prover.dispose();
});

test("verified cache entries avoid transport and corrupted entries are replaced", async () => {
  const { assets, manifest } = await fixture();
  const cache = new MemoryAssetCache();
  const firstTransport = new FakeTransport(assets);
  const firstWorker = new FakeWorker();
  const first = createWasmBeatExProver({
    manifest,
    assetBaseUrl: "https://assets.example/",
    assetTransport: firstTransport,
    assetCache: cache,
    workerFactory: () => firstWorker,
  });
  await first.proveDeposit({});
  first.dispose();

  const secondTransport = new FakeTransport(assets);
  const second = createWasmBeatExProver({
    manifest,
    assetBaseUrl: "https://assets.example/",
    assetTransport: secondTransport,
    assetCache: cache,
    workerFactory: () => new FakeWorker(),
  });
  await second.proveDeposit({});
  assert.deepEqual(secondTransport.loads, []);
  second.dispose();
});

test("asset store coalesces in-flight loads without retaining large settled buffers", async () => {
  const value = bytes(7, 8, 9);
  const asset = await descriptor("key.pk", value);
  let loads = 0;
  const store = new ProverAssetStore({
    transport: {
      async load() {
        loads += 1;
        await new Promise((resolve) => setTimeout(resolve, 1));
        return value.slice(0);
      },
    },
  });
  await Promise.all([store.load(asset), store.load(asset)]);
  assert.equal(loads, 1);
  await store.load(asset);
  assert.equal(loads, 2);
});

test("an asset with a mismatched digest never reaches the worker", async () => {
  const { assets, manifest } = await fixture();
  const worker = new FakeWorker();
  const corruptAssets = new Map(assets);
  corruptAssets.set("https://assets.example/balance.pk", bytes(9, 9));
  const prover = createWasmBeatExProver({
    manifest,
    assetBaseUrl: "https://assets.example/",
    assetTransport: new FakeTransport(corruptAssets),
    workerFactory: () => worker,
  });
  await assert.rejects(() => prover.proveDeposit({}), ProverAssetIntegrityError);
  assert.equal(worker.messages.some((message) => message.type === "load"), false);
  prover.dispose();
});

test("manifest pins the runtime backend and proving-key encoding", async () => {
  const { manifest } = await fixture();
  assert.throws(
    () => createWasmBeatExProver({
      manifest: { ...manifest, backend: "different-runtime" },
      workerFactory: () => new FakeWorker(),
    }),
    /Unsupported prover backend/,
  );
  assert.throws(
    () => createWasmBeatExProver({
      manifest: { ...manifest, provingKeyEncoding: "arkworks-canonical-compressed-v1" },
      workerFactory: () => new FakeWorker(),
    }),
    /Unsupported proving-key encoding/,
  );
});

test("abort sends cancellation to the worker and rejects the pending proof", async () => {
  const worker = new FakeWorker();
  worker.proveDelay = 30;
  const { prover } = await setup({ worker });
  const controller = new AbortController();
  const pending = prover.proveDeposit({}, { signal: controller.signal });
  setTimeout(() => controller.abort(), 5);
  await assert.rejects(pending, (error) => error.name === "AbortError");
  assert.ok(worker.messages.some((message) => message.type === "cancel"));
  prover.dispose();
});

test("wasm-bindgen runtime dispatches without JSON or number conversion", async () => {
  const calls = [];
  class FakeBindingsProver {
    loaded = new Set();
    loadProvingKey(circuit, value) {
      calls.push(["load", circuit, [...value]]);
      this.loaded.add(circuit);
    }
    isProvingKeyLoaded(circuit) { return this.loaded.has(circuit); }
    proveDeposit(request) { calls.push(["deposit", request]); return request; }
    proveWithdrawal(request) { return request; }
    proveOrder(request) { return request; }
    proveCancellation(request) { return request; }
    proveSettlement(request) { return request; }
    proveReplacement(request) { return request; }
    proveRollover(request) { return request; }
  }
  const runtime = await createTraderWasmRuntime(bytes(0), async () => ({
    default: async ({ module_or_path: wasm }) => calls.push(["initialize", wasm.byteLength]),
    traderProverApiVersion: () => 1,
    TraderProver: FakeBindingsProver,
  }));
  runtime.loadProvingKey("balance", bytes(1, 2));
  const request = { amount: 42n, salt: Uint8Array.of(1, 2, 3) };
  assert.equal(runtime.prove("deposit", request), request);
  assert.deepEqual(calls, [
    ["initialize", 1],
    ["load", "balance", [1, 2]],
    ["deposit", request],
  ]);
});
