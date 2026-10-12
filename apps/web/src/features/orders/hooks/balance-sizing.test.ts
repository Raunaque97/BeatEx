import { buildAuctionGrid } from "@beatex/sdk";
import { describe, expect, it, vi } from "vitest";

import { customSizing } from "./balance-sizing";
import type { OrderIntentState } from "./use-order-intent";

const grid = buildAuctionGrid({
  referencePrice: 80_000n,
  gridStepBps: 10,
  gridSize: 17,
});
const top = grid.reduce(
  (max, level) => (level.price > max ? level.price : max),
  0n,
);

function order(maxSell = "", maxBuy = "") {
  return {
    intent: { maxSell, maxBuy },
    setMaxSell: vi.fn(),
    setMaxBuy: vi.fn(),
  } as unknown as OrderIntentState;
}

describe("customSizing", () => {
  const balance = { baseAtoms: 5_000_000n, quoteAtoms: 5_000_000_000n };

  it("sizes Max sell from the tBTC balance in whole 100-satoshi units", () => {
    const current = order();
    const sizing = customSizing(
      current,
      { ...balance, baseAtoms: 1_234_567n },
      grid,
    );
    sizing.sell.size(100);
    expect(current.setMaxSell).toHaveBeenCalledWith("0.012345");
  });

  it("caps Max buy at what the tUSD covers at the top price level", () => {
    const current = order();
    const sizing = customSizing(current, balance, grid);
    // 5,000 tUSD buys this many 100-satoshi units at the highest level.
    expect(sizing.buy.available).toBe(5_000_000_000n / top);
    sizing.buy.size(100);
    const [maxBuy] = (current.setMaxBuy as ReturnType<typeof vi.fn>).mock
      .calls[0]! as [string];
    // A full buy at the top level must not need more than the balance.
    const units = BigInt(Math.round(Number(maxBuy) * 1e6));
    expect(units * top).toBeLessThanOrEqual(balance.quoteAtoms);
  });

  it("shows where typed maxima sit against the balance", () => {
    const sizing = customSizing(order("0.025", ""), balance, grid);
    expect(sizing.sell.percent).toBe(50);
    expect(sizing.buy.percent).toBe(0);
  });
});
