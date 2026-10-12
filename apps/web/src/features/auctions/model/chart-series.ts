import { AuctionState } from "@beatex/sdk";

import type { BatchSummary } from "./batch-summary";

export type PointSource = "clearing" | "reference";

export interface ChartPoint {
  readonly auctionId: bigint;
  /** Clearing price when the batch traded, otherwise the batch's reference price. */
  readonly price: bigint;
  readonly source: PointSource;
  /** Whether the reference came from Pyth (only meaningful for `reference`). */
  readonly fromOracle: boolean;
  readonly volume: bigint;
  readonly settled: boolean;
  readonly auction: BatchSummary;
}

/**
 * Price history: one point per known batch, newest last. A batch that
 * traded plots its clearing price; any other batch plots the reference price it
 * snapshotted, which is itself the Pyth price whenever the program fell back to the oracle.
 */
export function toChartPoints(
  auctions: Iterable<BatchSummary>,
  limit: number,
): readonly ChartPoint[] {
  const sorted = [...auctions].sort((left, right) =>
    left.auctionId < right.auctionId
      ? -1
      : left.auctionId > right.auctionId
        ? 1
        : 0,
  );
  return sorted.slice(-limit).map((auction) => {
    const settled = auction.state === AuctionState.Settled;
    const traded = settled && auction.clearingVolume > 0n;
    return {
      auctionId: auction.auctionId,
      price: traded ? auction.clearingPrice : auction.referencePrice,
      source: traded ? "clearing" : "reference",
      fromOracle: !traded && auction.referenceFromOracle,
      volume: settled ? auction.clearingVolume : 0n,
      settled,
      auction,
    };
  });
}

export interface PriceDomain {
  readonly min: number;
  readonly max: number;
}

/** A padded y-axis domain covering every price that must stay visible. */
export function priceDomain(
  prices: readonly bigint[],
  padding = 0.15,
): PriceDomain | null {
  if (prices.length === 0) return null;
  const values = prices.map(Number);
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = Math.max(max - min, max * 0.002);
  return { min: min - span * padding, max: max + span * padding };
}
