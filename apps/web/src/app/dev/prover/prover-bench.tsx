"use client";

import type { BeatExProver } from "@beatex/sdk";
import { useState } from "react";

import { getBrowserProver } from "@/lib/beatex/prover";

const bytes = (offset: number) =>
  Uint8Array.from({ length: 32 }, (_, index) => index + offset);
const committeeKey = Uint8Array.from(
  "20c796002be9d822432675db552615182c5a3dcb194e1a8bece31a3e73a68812".match(
    /../g,
  )!,
  (pair) => Number.parseInt(pair, 16),
);
const zero32 = new Uint8Array(32);
const curve = {
  buy: Array<bigint>(17).fill(0n),
  sell: Array<bigint>(17).fill(0n),
};

type Row = {
  readonly name: string;
  readonly coldMs: number;
  readonly warmMs: number;
};

/** Same fixed witnesses as packages/prover-wasm/test/wasm.integration.mjs. */
async function runBench(prover: BeatExProver, log: (row: Row) => void) {
  const time = async <T,>(task: () => Promise<T>) => {
    const started = performance.now();
    const result = await task();
    return { result, ms: performance.now() - started };
  };
  const twice = async <T,>(name: string, task: () => Promise<T>) => {
    const cold = await time(task);
    const warm = await time(task);
    log({ name, coldMs: cold.ms, warmMs: warm.ms });
    return warm.result;
  };

  await twice("deposit", () =>
    prover.proveDeposit({
      market: bytes(1),
      owner: bytes(33),
      keyEpoch: bytes(65),
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
    } as unknown as Parameters<BeatExProver["proveDeposit"]>[0]),
  );
  await twice("withdraw", () =>
    prover.proveWithdrawal({
      market: bytes(1),
      owner: bytes(33),
      keyEpoch: bytes(65),
      committeeKey,
      currentRevision: 1n,
      baseAmount: 40n,
      quoteAmount: 4_000n,
      currentBalance: {
        baseValue: 100n,
        quoteValue: 10_000n,
        baseRandomness: zero32,
        quoteRandomness: zero32,
      },
    } as unknown as Parameters<BeatExProver["proveWithdrawal"]>[0]),
  );
  const order = await twice("order", () =>
    prover.proveOrder({
      auction: bytes(1),
      owner: bytes(33),
      clientOrderNonce: 7n,
      referencePrice: 100_000n,
      gridStepBps: 10,
      committeeKey,
      curve,
      oldBalance: {
        baseValue: 10n,
        quoteValue: 1_000_000n,
        baseRandomness: zero32,
        quoteRandomness: zero32,
      },
    } as unknown as Parameters<BeatExProver["proveOrder"]>[0]),
  );
  await twice("settlement", () =>
    prover.proveSettlement({
      market: bytes(65),
      auction: bytes(1),
      order: bytes(129),
      activeOrder: bytes(97),
      owner: bytes(33),
      keyEpoch: bytes(161),
      clientOrderNonce: 7n,
      auctionSequence: 0n,
      allocationRuleVersion: 2,
      orderCircuitVersion: 3,
      claimCircuitVersion: 3,
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
    } as unknown as Parameters<BeatExProver["proveSettlement"]>[0]),
  );
}

export function ProverBench() {
  const [rows, setRows] = useState<Row[]>([]);
  const [status, setStatus] = useState("idle");

  const start = async () => {
    setRows([]);
    setStatus("loading prover…");
    try {
      const prover = await getBrowserProver();
      setStatus("proving…");
      await runBench(prover as unknown as BeatExProver, (row) =>
        setRows((current) => [...current, row]),
      );
      setStatus("done");
    } catch (error) {
      setStatus(
        `failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  };

  return (
    <main className="mx-auto flex max-w-xl flex-col gap-4 p-8">
      <h1 className="text-xl font-semibold">Browser prover benchmark</h1>
      <p className="text-muted text-sm">
        Runs each trader proof twice in the app&apos;s Web Worker. Cold includes
        fetching and verifying that circuit&apos;s proving key.
      </p>
      <button
        type="button"
        onClick={() => void start()}
        className="bg-accent text-accent-ink min-h-11 self-start rounded-lg px-5 font-semibold"
      >
        Run benchmark
      </button>
      <p data-testid="bench-status" className="font-mono text-sm">
        {status}
      </p>
      <table data-testid="bench-results" className="font-mono text-sm">
        <thead className="text-muted text-left">
          <tr>
            <th>proof</th>
            <th>cold ms</th>
            <th>warm ms</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.name}>
              <td>{row.name}</td>
              <td>{row.coldMs.toFixed(0)}</td>
              <td>{row.warmMs.toFixed(0)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </main>
  );
}
