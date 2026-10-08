import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import test from "node:test";
import init, {
  TraderProver,
  traderProverApiVersion,
} from "../dist/generated/beatex_trader_wasm_prover.js";

const assetDirectory = process.env.BEATEX_PROVER_ASSETS;
const committeeKey = Uint8Array.from(
  Buffer.from("20c796002be9d822432675db552615182c5a3dcb194e1a8bece31a3e73a68812", "hex"),
);

test(
  "production WASM loads the Devnet balance key and generates a real deposit proof",
  { skip: assetDirectory === undefined ? "set BEATEX_PROVER_ASSETS to generated assets" : false },
  async (context) => {
    const directory = resolve(assetDirectory);
    const wasm = await readFile(resolve(directory, "beatex_trader_wasm_prover_bg.wasm"));
    await init({ module_or_path: wasm });
    assert.equal(traderProverApiVersion(), 1);

    const prover = new TraderProver();
    const key = await readFile(resolve(directory, "balance-v1.pk"));
    const keyLoadStarted = performance.now();
    prover.loadProvingKey("balance", key);
    const keyLoadMs = performance.now() - keyLoadStarted;
    assert.equal(prover.isProvingKeyLoaded("balance"), true);

    const zero32 = new Uint8Array(32);
    const output = prover.proveDeposit({
      market: Uint8Array.from({ length: 32 }, (_, index) => index + 1),
      owner: Uint8Array.from({ length: 32 }, (_, index) => index + 33),
      keyEpoch: Uint8Array.from({ length: 32 }, (_, index) => index + 65),
      committeeKey,
      oldRevision: 0n,
      baseAmount: 100n,
      quoteAmount: 10_000n,
      oldBalance: {
        baseValue: 0n,
        quoteValue: 0n,
        baseRandomness: zero32,
        quoteRandomness: zero32,
      },
    });

    assert.equal(output.backend, "beatex-arkworks-groth16-wasm-v1");
    assert.equal(output.circuitVersion, 1);
    assert.equal(output.newBalance.baseValue, 100n);
    assert.equal(output.newBalance.quoteValue, 10_000n);
    assert.equal(output.proof.proofA.length, 64);
    assert.equal(output.proof.proofB.length, 128);
    assert.equal(output.proof.proofC.length, 64);
    assert.equal(output.proof.publicInput.length, 32);
    assert.notDeepEqual(output.proof.publicInput, zero32);
    assert.ok(output.provingTimeMs > 0);

    const withdrawal = prover.proveWithdrawal({
      market: Uint8Array.from({ length: 32 }, (_, index) => index + 1),
      owner: Uint8Array.from({ length: 32 }, (_, index) => index + 33),
      keyEpoch: Uint8Array.from({ length: 32 }, (_, index) => index + 65),
      committeeKey,
      currentRevision: 1n,
      baseAmount: 40n,
      quoteAmount: 4_000n,
      currentBalance: output.newBalance,
    });
    assert.equal(withdrawal.remainingBalance.baseValue, 60n);
    assert.equal(withdrawal.remainingBalance.quoteValue, 6_000n);
    assert.equal(withdrawal.proof.proofA.length, 64);
    context.diagnostic(
      `balance key load ${keyLoadMs.toFixed(1)} ms; deposit ${output.provingTimeMs.toFixed(1)} ms; withdrawal ${withdrawal.provingTimeMs.toFixed(1)} ms`,
    );
    prover.free();
  },
);

test(
  "production WASM generates order, release, and settlement proofs with one runtime",
  { skip: assetDirectory === undefined ? "set BEATEX_PROVER_ASSETS to generated assets" : false },
  async (context) => {
    const directory = resolve(assetDirectory);
    const wasm = await readFile(resolve(directory, "beatex_trader_wasm_prover_bg.wasm"));
    await init({ module_or_path: wasm });
    const prover = new TraderProver();
    const keyLoadTimes = [];
    for (const [circuit, file] of [
      ["order", "order-v1.pk"],
      ["release", "release-v1.pk"],
      ["settlement", "settlement-v1.pk"],
    ]) {
      const started = performance.now();
      prover.loadProvingKey(circuit, await readFile(resolve(directory, file)));
      keyLoadTimes.push(`${circuit} ${(performance.now() - started).toFixed(1)} ms`);
    }

    const auction = Uint8Array.from({ length: 32 }, (_, index) => index + 1);
    const owner = Uint8Array.from({ length: 32 }, (_, index) => index + 33);
    const market = Uint8Array.from({ length: 32 }, (_, index) => index + 65);
    const activeOrder = Uint8Array.from({ length: 32 }, (_, index) => index + 97);
    const orderAddress = Uint8Array.from({ length: 32 }, (_, index) => index + 129);
    const keyEpoch = Uint8Array.from({ length: 32 }, (_, index) => index + 161);
    const zero32 = new Uint8Array(32);
    const curve = { buy: Array(17).fill(0n), sell: Array(17).fill(0n) };
    const initialBalance = {
      baseValue: 10n,
      quoteValue: 1_000_000n,
      baseRandomness: zero32,
      quoteRandomness: zero32,
    };
    const order = prover.proveOrder({
      auction,
      owner,
      clientOrderNonce: 7n,
      referencePrice: 100_000n,
      tickSize: 500n,
      committeeKey,
      curve,
      oldBalance: initialBalance,
    });
    assert.equal(order.ciphertexts.length, 34);
    assert.equal(order.newBalance.baseValue, 10n);
    assert.equal(order.newBalance.quoteValue, 1_000_000n);

    const release = prover.proveCancellation({
      market,
      auction,
      activeOrder,
      owner,
      balanceRevision: 1n,
      clientOrderNonce: 7n,
      curve,
      baseReserve: order.baseReserve,
      quoteReserve: order.quoteReserve,
      orderSalt: order.orderSalt,
      expectedOrderCommitment: order.orderCommitment,
      committeeKey,
      oldBalance: order.newBalance,
    });
    assert.equal(release.releasedBalance.baseValue, 10n);
    assert.equal(release.releasedBalance.quoteValue, 1_000_000n);

    const settlement = prover.proveSettlement({
      market,
      auction,
      order: orderAddress,
      activeOrder,
      owner,
      keyEpoch,
      clientOrderNonce: 7n,
      auctionSequence: 0n,
      allocationRuleVersion: 1,
      orderCircuitVersion: 1,
      claimCircuitVersion: 1,
      curve,
      baseReserve: order.baseReserve,
      quoteReserve: order.quoteReserve,
      orderSalt: order.orderSalt,
      expectedOrderCommitment: order.orderCommitment,
      clearingIndex: 8,
      clearingPrice: 100_000n,
      demandAtClearing: 0n,
      supplyAtClearing: 0n,
      clearingVolume: 0n,
      balanceRevision: 1n,
      committeeKey,
      oldBalance: order.newBalance,
    });
    assert.equal(settlement.creditedBalance.baseValue, 10n);
    assert.equal(settlement.creditedBalance.quoteValue, 1_000_000n);

    const releaseRequest = {
      market,
      auction,
      activeOrder,
      owner,
      balanceRevision: 1n,
      clientOrderNonce: 7n,
      curve,
      baseReserve: order.baseReserve,
      quoteReserve: order.quoteReserve,
      orderSalt: order.orderSalt,
      expectedOrderCommitment: order.orderCommitment,
      committeeKey,
      oldBalance: order.newBalance,
    };
    const nextOrder = {
      auction,
      owner,
      clientOrderNonce: 8n,
      referencePrice: 100_000n,
      tickSize: 500n,
      committeeKey,
      curve,
    };
    const replacement = prover.proveReplacement({
      release: releaseRequest,
      replacement: nextOrder,
    });
    assert.equal(replacement.replacement.newBalance.baseValue, 10n);
    const rollover = prover.proveRollover({
      settlement: {
        market,
        auction,
        order: orderAddress,
        activeOrder,
        owner,
        keyEpoch,
        clientOrderNonce: 7n,
        auctionSequence: 0n,
        allocationRuleVersion: 1,
        orderCircuitVersion: 1,
        claimCircuitVersion: 1,
        curve,
        baseReserve: order.baseReserve,
        quoteReserve: order.quoteReserve,
        orderSalt: order.orderSalt,
        expectedOrderCommitment: order.orderCommitment,
        clearingIndex: 8,
        clearingPrice: 100_000n,
        demandAtClearing: 0n,
        supplyAtClearing: 0n,
        clearingVolume: 0n,
        balanceRevision: 1n,
        committeeKey,
        oldBalance: order.newBalance,
      },
      nextOrder,
    });
    assert.equal(rollover.nextOrder.newBalance.baseValue, 10n);
    context.diagnostic(
      `key loads: ${keyLoadTimes.join(", ")}`,
    );
    context.diagnostic(
      `order ${order.provingTimeMs.toFixed(1)} ms; release ${release.provingTimeMs.toFixed(1)} ms; settlement ${settlement.provingTimeMs.toFixed(1)} ms; replacement ${replacement.provingTimeMs.toFixed(1)} ms; rollover ${rollover.provingTimeMs.toFixed(1)} ms`,
    );
    prover.free();
  },
);
