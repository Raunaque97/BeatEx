import { buildAuctionGrid } from "@beatex/sdk";
import { describe, expect, it, vi } from "vitest";

import type { MarketConfig } from "@/lib/config/markets";

import { customSizing, spendSizing } from "./balance-sizing";
import type { OrderIntentState } from "./use-order-intent";

const tbtc: MarketConfig = {
  address: "tbtc-market",
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
  defaultCurveMax: "0.1",
};
const beat: MarketConfig = {
  address: "beat-market",
  slug: "beat-tusd",
  label: "BEAT / tUSD",
  base: { symbol: "BEAT", decimals: 2 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: false,
  defaultCurveMax: "1000",
};

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
      tbtc,
      { ...balance, baseAtoms: 1_234_567n },
      grid,
    );
    sizing.sell.size(100);
    expect(current.setMaxSell).toHaveBeenCalledWith("0.012345");
  });

  it("caps Max buy at what the tUSD covers at the top price level", () => {
    const current = order();
    const sizing = customSizing(current, tbtc, balance, grid);
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
    const sizing = customSizing(order("0.025", ""), tbtc, balance, grid);
    expect(sizing.sell.percent).toBe(50);
    expect(sizing.buy.percent).toBe(0);
  });

  it("sizes BEAT in whole tokens, since a base unit is one BEAT", () => {
    const beatGrid = buildAuctionGrid({
      referencePrice: 1_000_000n,
      gridStepBps: 10,
      gridSize: 17,
    });
    const current = order();
    const sizing = customSizing(
      current,
      beat,
      { baseAtoms: 12_345n, quoteAtoms: 1_000_000_000n },
      beatGrid,
    );
    sizing.sell.size(100);
    // 123.45 BEAT held, of which 123 whole units can trade.
    expect(current.setMaxSell).toHaveBeenCalledWith("123");
    // 1,000 tUSD at the top level of 1.008 tUSD per BEAT.
    expect(sizing.buy.available).toBe(992n);
    sizing.buy.size(100);
    expect(current.setMaxBuy).toHaveBeenCalledWith("992");
  });

  it("reads typed BEAT maxima against the balance", () => {
    const sizing = customSizing(
      order("50", ""),
      beat,
      { baseAtoms: 10_000n, quoteAtoms: 0n },
      grid,
    );
    expect(sizing.sell.percent).toBe(50);
  });
});

describe("spendSizing", () => {
  function spendOrder(amount: string) {
    return {
      intent: { side: "sell", unit: "base", amount },
      built: null,
      setAmount: vi.fn(),
      setUnit: vi.fn(),
    } as unknown as OrderIntentState;
  }

  it("sizes a tBTC sell in 100-satoshi units", () => {
    const current = spendOrder("");
    spendSizing(current, tbtc, {
      baseAtoms: 1_234_567n,
      quoteAtoms: 0n,
    }).size(100);
    expect(current.setAmount).toHaveBeenCalledWith("0.012345");
  });

  it("sizes a BEAT sell in whole BEAT", () => {
    const current = spendOrder("");
    spendSizing(current, beat, { baseAtoms: 12_345n, quoteAtoms: 0n }).size(
      100,
    );
    expect(current.setAmount).toHaveBeenCalledWith("123");
  });

  it("shows a typed BEAT amount as a share of the balance", () => {
    const sizing = spendSizing(spendOrder("25"), beat, {
      baseAtoms: 10_000n,
      quoteAtoms: 0n,
    });
    expect(sizing.percent).toBe(25);
  });
});
