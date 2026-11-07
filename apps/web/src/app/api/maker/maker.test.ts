import { buildAuctionGrid, type MarketAccount } from "@beatex/sdk";
import { describe, expect, it } from "vitest";

import { makerCurve, makerDeposit, makerSeed, nextAuctionId } from "./maker";

const tbtcGrid = buildAuctionGrid({
  referencePrice: 76_000n,
  gridStepBps: 10,
  gridSize: 17,
});
const beatGrid = buildAuctionGrid({
  referencePrice: 1_000_000n,
  gridStepBps: 10,
  gridSize: 17,
});
const LEVEL_QUOTE = 500_000_000n;
const NO_LIMIT = 1n << 40n;

describe("makerCurve", () => {
  it("bids below the reference, asks above it, and leaves the centre empty", () => {
    const curve = makerCurve(tbtcGrid, LEVEL_QUOTE, NO_LIMIT);

    expect(curve.buy[8]).toBe(0n);
    expect(curve.sell[8]).toBe(0n);
    expect(curve.buy.slice(0, 8).every((quantity) => quantity > 0n)).toBe(true);
    expect(curve.sell.slice(9).every((quantity) => quantity > 0n)).toBe(true);
    expect(curve.buy.slice(9).every((quantity) => quantity === 0n)).toBe(true);
    expect(curve.sell.slice(0, 8).every((quantity) => quantity === 0n)).toBe(
      true,
    );
  });

  it("grows away from the centre up to twice the nearest level", () => {
    const { sell } = makerCurve(tbtcGrid, LEVEL_QUOTE, NO_LIMIT);

    expect(sell[10]!).toBeGreaterThan(sell[9]!);
    expect(sell[16]!).toBeLessThanOrEqual(sell[9]! * 2n);
  });

  it("sizes by quote value, so each market quotes a similar amount", () => {
    const tbtc = makerCurve(tbtcGrid, LEVEL_QUOTE, NO_LIMIT);
    const beat = makerCurve(beatGrid, LEVEL_QUOTE, NO_LIMIT);

    // 500 tUSD: about 0.0066 tBTC (100-satoshi units), and about 500 BEAT.
    expect(tbtc.sell[9]).toBe(6_572n);
    expect(beat.sell[9]).toBe(499n);
  });

  it("never exceeds the market's maximum order quantity", () => {
    const curve = makerCurve(tbtcGrid, LEVEL_QUOTE, 1_000n);

    expect([...curve.buy, ...curve.sell].every((q) => q <= 1_000n)).toBe(true);
  });
});

describe("makerDeposit", () => {
  it("covers the largest curve entry in both assets, with headroom", () => {
    const curve = makerCurve(tbtcGrid, LEVEL_QUOTE, NO_LIMIT);
    const { baseAtoms, quoteAtoms } = makerDeposit(76_000n, LEVEL_QUOTE);
    const largestSell = curve.sell.reduce((a, b) => (a > b ? a : b));
    const largestCost = curve.buy
      .map((quantity, index) => quantity * tbtcGrid[index]!.price)
      .reduce((a, b) => (a > b ? a : b));

    expect(baseAtoms).toBeGreaterThan(largestSell * 100n);
    expect(quoteAtoms).toBeGreaterThan(largestCost);
  });
});

describe("nextAuctionId", () => {
  const market = {
    marketStartSlot: 1_000n,
    submissionDurationSlots: 40n,
    aggregationDurationSlots: 200n,
    decryptionDurationSlots: 200n,
  } as MarketAccount;

  it("targets the first auction to open once the maker is ready", () => {
    expect(nextAuctionId(market, 1_000n)).toBe(2n);
    expect(nextAuctionId(market, 1_019n)).toBe(2n);
    expect(nextAuctionId(market, 1_020n)).toBe(3n);
    expect(nextAuctionId(market, 1_039n)).toBe(3n);
  });

  it("has no target before the market starts", () => {
    expect(nextAuctionId(market, 999n)).toBeNull();
  });
});

describe("makerSeed", () => {
  const secret = new Uint8Array(64).fill(7);

  it("is stable, so concurrent calls agree on the auction's wallet", () => {
    expect(makerSeed(secret, "market", 5n)).toStrictEqual(
      makerSeed(secret, "market", 5n),
    );
  });

  it("differs per auction and per market", () => {
    const seed = makerSeed(secret, "market", 5n);

    expect(seed).not.toStrictEqual(makerSeed(secret, "market", 6n));
    expect(seed).not.toStrictEqual(makerSeed(secret, "other", 5n));
    expect(seed).toHaveLength(32);
  });
});
