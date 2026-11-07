import { buildAuctionGrid } from "@beatex/sdk";
import { describe, expect, it } from "vitest";

import { limitLevel, remainingOrder, type WorkingLimit } from "./working-limit";

const grid = buildAuctionGrid({
  referencePrice: 76_000n,
  gridStepBps: 10,
  gridSize: 17,
});
const NO_LIMIT = 1n << 40n;
const market = {
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
};
const beat = {
  base: { symbol: "BEAT", decimals: 2 },
  quote: { symbol: "tUSD", decimals: 6 },
};

describe("limitLevel", () => {
  it("finds a price that is on the grid", () => {
    expect(limitLevel(grid, "buy", grid[3]!.price)).toBe(3);
    expect(limitLevel(grid, "sell", grid[3]!.price)).toBe(3);
  });

  it("never trades past the trader's price when the grid has moved", () => {
    const between = grid[3]!.price + 1n;

    expect(limitLevel(grid, "buy", between)).toBe(3);
    expect(limitLevel(grid, "sell", between)).toBe(4);
  });

  it("uses every level when the whole grid is better than the price", () => {
    expect(limitLevel(grid, "buy", grid[16]!.price + 1n)).toBe(16);
    expect(limitLevel(grid, "sell", grid[0]!.price - 1n)).toBe(0);
  });

  it("sits out when the whole grid is worse than the price", () => {
    expect(limitLevel(grid, "buy", grid[0]!.price - 1n)).toBeNull();
    expect(limitLevel(grid, "sell", grid[16]!.price + 1n)).toBeNull();
  });
});

describe("remainingOrder", () => {
  const working: WorkingLimit = {
    side: "buy",
    limitPrice: grid[10]!.price,
    total: 1_000n,
    filled: 400n,
    auctionId: null,
  };

  it("asks only for what is left, up to the trader's price", () => {
    const built = remainingOrder(working, grid, NO_LIMIT, market);
    if (built === null || !built.ok) throw new Error("expected an order");

    expect(built.order.curve.buy[10]).toBe(600n);
    expect(built.order.curve.buy[11]).toBe(0n);
    expect(built.order.curve.sell.every((quantity) => quantity === 0n)).toBe(
      true,
    );
  });

  it("waits when no level can honour the price", () => {
    expect(
      remainingOrder(
        { ...working, limitPrice: grid[0]!.price - 1n },
        grid,
        NO_LIMIT,
        market,
      ),
    ).toBeNull();
  });

  it("reads what is left in the market's own units", () => {
    const built = remainingOrder(working, grid, NO_LIMIT, beat);
    if (built === null || !built.ok) throw new Error("expected an order");

    expect(built.order.curve.buy[10]).toBe(600n);
  });
});
