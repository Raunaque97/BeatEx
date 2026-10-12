import { buildAuctionGrid } from "@beatex/sdk";
import { describe, expect, it } from "vitest";

import {
  buildOrder,
  emptyCustomLevels,
  estimateOutcome,
  fillAt,
  levelIsActive,
  type OrderIntent,
} from "./order-curve";

const grid = buildAuctionGrid({
  referencePrice: 85_500n,
  gridStepBps: 10,
  gridSize: 17,
});
const MAX = 1_000_000n;

function intent(overrides: Partial<OrderIntent>): OrderIntent {
  return {
    type: "market",
    side: "buy",
    amount: "1000",
    unit: "quote",
    limitIndex: 7,
    customLevels: emptyCustomLevels(),
    maxBuy: "0.1",
    maxSell: "0.05",
    ...overrides,
  };
}

function built(value: OrderIntent) {
  const result = buildOrder(value, grid, MAX);
  if (!result.ok) throw new Error(result.error);
  return result.order;
}

describe("buildOrder", () => {
  it("spends a quote budget at every level without exceeding it", () => {
    const order = built(intent({}));
    expect(order.curve.sell.every((quantity) => quantity === 0n)).toBe(true);
    order.curve.buy.forEach((quantity, index) => {
      expect(quantity).toBe(1_000_000_000n / grid[index]!.price);
      expect(quantity * grid[index]!.price).toBeLessThanOrEqual(1_000_000_000n);
    });
    expect(order.quoteReserve).toBeLessThanOrEqual(1_000_000_000n);
    expect(order.baseReserve).toBe(0n);
  });

  it("sells a fixed base amount at every level for a market sell", () => {
    const order = built(intent({ side: "sell", unit: "base", amount: "0.05" }));
    expect(order.curve.sell.every((quantity) => quantity === 50_000n)).toBe(
      true,
    );
    expect(order.baseReserve).toBe(50_000n);
    expect(order.quoteReserve).toBe(0n);
  });

  it("limits a buy to levels at or below the limit and a sell at or above", () => {
    const buy = built(
      intent({ type: "limit", unit: "base", amount: "0.01", limitIndex: 7 }),
    );
    expect(buy.curve.buy.map((quantity) => quantity > 0n)).toEqual(
      grid.map((level) => level.index <= 7),
    );
    expect(buy.quoteReserve).toBe(10_000n * grid[7]!.price);

    const sell = built(
      intent({
        type: "limit",
        side: "sell",
        unit: "base",
        amount: "0.01",
        limitIndex: 9,
      }),
    );
    expect(sell.curve.sell.map((quantity) => quantity > 0n)).toEqual(
      grid.map((level) => level.index >= 9),
    );
  });

  it("scales custom percentages by the max buy and max sell amounts", () => {
    // maxBuy 0.1 tBTC = 100,000 units; maxSell 0.05 tBTC = 50,000 units.
    const customLevels = emptyCustomLevels().map((_, index) =>
      index < 8 ? 100 : index > 8 ? -50 : 0,
    );
    const order = built(intent({ type: "custom", customLevels }));
    expect(order.curve.buy.slice(0, 8)).toEqual(Array(8).fill(100_000n));
    expect(order.curve.sell.slice(9)).toEqual(Array(8).fill(25_000n));
    order.curve.buy.forEach((quantity, index) =>
      expect(quantity === 0n || order.curve.sell[index] === 0n).toBe(true),
    );
    expect(order.baseReserve).toBe(25_000n);
    expect(order.quoteReserve).toBe(100_000n * grid[7]!.price);
  });

  it("requires a valid max only for the sides a custom curve uses", () => {
    const buysOnly = emptyCustomLevels().map((_, index) =>
      index < 8 ? 40 : 0,
    );
    expect(
      buildOrder(
        intent({ type: "custom", customLevels: buysOnly, maxSell: "" }),
        grid,
        MAX,
      ),
    ).toMatchObject({ ok: true });
    expect(
      buildOrder(
        intent({ type: "custom", customLevels: buysOnly, maxBuy: "" }),
        grid,
        MAX,
      ),
    ).toMatchObject({ ok: false, error: "Set a max buy amount." });
    expect(
      buildOrder(
        intent({ type: "custom", customLevels: buysOnly, maxBuy: "5" }),
        grid,
        MAX,
      ),
    ).toMatchObject({
      ok: false,
      error: "This market allows at most 1 tBTC per price level.",
    });
  });

  it("rejects bad, empty, too-fine, and oversized amounts", () => {
    expect(buildOrder(intent({ amount: "abc" }), grid, MAX)).toMatchObject({
      ok: false,
    });
    expect(buildOrder(intent({ amount: "0" }), grid, MAX)).toMatchObject({
      ok: false,
    });
    expect(
      buildOrder(intent({ unit: "base", amount: "0.00000001" }), grid, MAX),
    ).toMatchObject({ ok: false, error: "tBTC amounts go down to 0.000001." });
    expect(
      buildOrder(intent({ unit: "base", amount: "2" }), grid, MAX),
    ).toMatchObject({
      ok: false,
    });
    expect(buildOrder(intent({ type: "custom" }), grid, MAX)).toMatchObject({
      ok: false,
    });
    expect(buildOrder(intent({}), [], MAX)).toMatchObject({ ok: false });
  });
});

describe("estimateOutcome", () => {
  it("estimates at the reference level with a range over every possible level", () => {
    const order = built(intent({}));
    const estimate = estimateOutcome(order, grid, "buy")!;
    expect(estimate.index).toBe(8);
    expect(estimate.receive).toBe(1_000_000_000n / 85_500n);
    expect(estimate.receiveMin).toBe(1_000_000_000n / grid[16]!.price);
    expect(estimate.receiveMax).toBe(1_000_000_000n / grid[0]!.price);
  });

  it("uses the nearest traded level when the limit excludes the reference", () => {
    const order = built(
      intent({
        type: "limit",
        side: "sell",
        unit: "base",
        amount: "0.01",
        limitIndex: 10,
      }),
    );
    const estimate = estimateOutcome(order, grid, "sell")!;
    expect(estimate.index).toBe(10);
    expect(estimate.receive).toBe(10_000n * grid[10]!.price);
    expect(estimate.pay).toBe(10_000n);
  });
});

describe("levelIsActive and fillAt", () => {
  it("reports the levels an intent trades at and the fill at one index", () => {
    expect(levelIsActive(intent({ type: "limit", limitIndex: 3 }), 4)).toBe(
      false,
    );
    expect(
      levelIsActive(intent({ type: "limit", side: "sell", limitIndex: 3 }), 4),
    ).toBe(true);
    const order = built(intent({ unit: "base", amount: "0.01" }));
    expect(fillAt(order.curve, grid, 8)).toEqual({
      base: 10_000n,
      quote: 10_000n * 85_500n,
    });
  });
});
