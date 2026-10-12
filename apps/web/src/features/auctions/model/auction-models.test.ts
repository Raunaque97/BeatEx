import {
  AuctionState,
  type AuctionAccount,
  type MarketAccount,
} from "@beatex/sdk";
import { describe, expect, it } from "vitest";

import { priceDomain, toChartPoints } from "./chart-series";
import { describeCurrentBatch } from "./current-batch";

const market = {
  marketStartSlot: 1_000n,
  submissionDurationSlots: 20n,
  aggregationDurationSlots: 200n,
  decryptionDurationSlots: 200n,
  lastClearedAuctionSequence: 0n,
  lastClearingVolume: 0n,
  lastClearingPrice: 0n,
  minimumReferenceVolume: 1n,
  oracleExponentAdjustment: 0,
  gridStepBps: 10,
  gridRadius: 8,
} as MarketAccount;

function auction(overrides: Partial<AuctionAccount>): AuctionAccount {
  return {
    auctionId: 5n,
    state: AuctionState.Pending,
    openSlot: 1_100n,
    submissionEndSlot: 1_120n,
    aggregationDeadlineSlot: 1_320n,
    decryptionDeadlineSlot: 1_520n,
    referencePrice: 85_500n,
    referenceFromOracle: true,
    gridStepBps: 10,
    gridSize: 17,
    clearingPrice: 0n,
    clearingVolume: 0n,
    ...overrides,
  } as AuctionAccount;
}

const oracle = {
  feedId: new Uint8Array(32),
  price: 8_296_400_000_000n,
  conf: 0n,
  exponent: -8,
  publishTime: 0n,
  postedSlot: 0n,
};

describe("describeCurrentBatch", () => {
  it("uses the Auction account's grid when it exists", () => {
    const batch = describeCurrentBatch(market, 1_105n, auction({}), oracle)!;
    expect(batch.auctionId).toBe(5n);
    expect(batch.phase).toBe("collecting");
    expect(batch.slotsLeft).toBe(15n);
    expect(batch.progress).toBeCloseTo(0.25);
    expect(batch.grid?.[8]?.price).toBe(85_500n);
    expect(batch.gridIsPreview).toBe(false);
  });

  it("previews the Pyth-based grid before the auction is initialized", () => {
    const batch = describeCurrentBatch(market, 1_105n, null, oracle)!;
    expect(batch.phase).toBe("not-started");
    expect(batch.referencePrice).toBe(82_964n);
    expect(batch.referenceSource).toBe("oracle");
    expect(batch.grid?.[8]?.price).toBe(82_964n);
    expect(batch.gridIsPreview).toBe(true);
  });

  it("has no grid without an auction or an oracle price, and no batch before start", () => {
    expect(describeCurrentBatch(market, 1_105n, null, null)?.grid).toBeNull();
    expect(describeCurrentBatch(market, 999n, null, oracle)).toBeNull();
  });
});

describe("toChartPoints", () => {
  it("plots clearing prices for trades and reference prices otherwise", () => {
    const points = toChartPoints(
      [
        auction({
          auctionId: 9n,
          state: AuctionState.Settled,
          clearingPrice: 85_586n,
          clearingVolume: 3n,
        }),
        auction({
          auctionId: 7n,
          state: AuctionState.Settled,
          referencePrice: 85_400n,
          clearingVolume: 0n,
        }),
        auction({
          auctionId: 8n,
          referencePrice: 85_450n,
          referenceFromOracle: false,
        }),
      ],
      2,
    );
    expect(points.map((point) => point.auctionId)).toEqual([8n, 9n]);
    expect(points[0]).toMatchObject({
      price: 85_450n,
      source: "reference",
      fromOracle: false,
      settled: false,
    });
    expect(points[1]).toMatchObject({
      price: 85_586n,
      source: "clearing",
      volume: 3n,
      settled: true,
    });
  });

  it("pads the price domain", () => {
    expect(priceDomain([])).toBeNull();
    const domain = priceDomain([100n, 200n])!;
    expect(domain.min).toBeLessThan(100);
    expect(domain.max).toBeGreaterThan(200);
  });
});
