import { AuctionState } from "@beatex/sdk";
import { describe, expect, it } from "vitest";

import type { BatchSummary } from "./batch-summary";
import { toChartPoints } from "./chart-series";
import {
  formatTick,
  placeInTime,
  secondsPerAuction,
  timeTicks,
  toCandles,
} from "./price-timeline";

function auction(
  id: number,
  price: number,
  blockTime: number | null,
  volume = 1,
): BatchSummary {
  return {
    auctionId: BigInt(id),
    state: AuctionState.Settled,
    referencePrice: BigInt(price),
    referenceFromOracle: false,
    gridStepBps: 10,
    gridSize: 17,
    clearingIndex: 8,
    clearingPrice: BigInt(price),
    clearingVolume: BigInt(volume),
    decryptedDemand: [],
    decryptedSupply: [],
    blockTime,
  };
}

describe("placing auctions in time", () => {
  it("measures the auction rate from timed neighbours", () => {
    const points = toChartPoints(
      [auction(1, 100, 1_000), auction(2, 100, 1_010), auction(4, 100, 1_030)],
      10,
    );
    expect(secondsPerAuction(points)).toBe(10);
  });

  it("keeps block times and estimates the rest from the nearest timed auction", () => {
    const points = toChartPoints(
      [auction(1, 100, 1_000), auction(2, 100, 1_010), auction(5, 100, null)],
      10,
    );
    expect(placeInTime(points, null).map((point) => point.time)).toEqual([
      1_000, 1_010, 1_040,
    ]);
  });

  it("anchors to now for the live auction when nothing is timed", () => {
    const points = toChartPoints([auction(9, 100, null)], 10);
    expect(placeInTime(points, { auctionId: 10n, now: 5_000 })[0]!.time).toBe(
      4_990,
    );
  });
});

describe("1-minute candles", () => {
  it("takes open, high, low, close, and summed volume per minute", () => {
    const points = placeInTime(
      toChartPoints(
        [
          auction(1, 100, 60, 2),
          auction(2, 130, 70, 3),
          auction(3, 90, 80, 0),
          auction(4, 110, 110, 5),
          auction(5, 120, 125, 1),
        ],
        10,
      ),
      null,
    );
    expect(toCandles(points)).toEqual([
      {
        time: 60,
        open: 100n,
        high: 130n,
        low: 90n,
        close: 110n,
        volume: 10n,
        count: 4,
      },
      {
        time: 120,
        open: 120n,
        high: 120n,
        low: 120n,
        close: 120n,
        volume: 1n,
        count: 1,
      },
    ]);
  });
});

describe("time axis", () => {
  it("picks round ticks that fit", () => {
    expect(timeTicks(0, 600, 7)).toEqual([0, 120, 240, 360, 480, 600]);
    expect(timeTicks(30, 100, 7)).toEqual([30, 45, 60, 75, 90]);
  });

  it("labels in hours and minutes, with seconds for fine ticks", () => {
    const time = new Date(2026, 9, 13, 9, 5, 30).getTime() / 1_000;
    expect(formatTick(time, 60)).toBe("09:05");
    expect(formatTick(time, 15)).toBe("09:05:30");
  });
});

describe("out-of-order publication times", () => {
  it("keeps times non-decreasing and one candle per minute", () => {
    const points = placeInTime(
      toChartPoints(
        [auction(1, 100, 70), auction(2, 110, 55), auction(3, 120, 130)],
        10,
      ),
      null,
    );
    expect(points.map((point) => point.time)).toEqual([70, 70, 130]);
    expect(toCandles(points).map((candle) => candle.time)).toEqual([60, 120]);
  });
});
