import type { AuctionGridLevel } from "@beatex/sdk";

import {
  buildOrder,
  emptyCustomLevels,
  type BuildResult,
  type OrderSide,
} from "@/features/orders/model/order-curve";
import { baseUnitsToTokens, type MarketUnits } from "@/lib/format";

/** A limit order this tab keeps submitting until it has fully filled. */
export interface WorkingLimit {
  readonly side: OrderSide;
  /** The trader's price: the grid moves between auctions, this does not. */
  readonly limitPrice: bigint;
  /** Base units to trade in all. */
  readonly total: bigint;
  /** Base units traded so far. */
  readonly filled: bigint;
  /** The auction holding the order, or null between auctions. */
  readonly auctionId: bigint | null;
}

/**
 * The level to trade at: the highest at or below a buy's price, the lowest at or
 * above a sell's. Null when the whole grid is on the wrong side of the price.
 */
export function limitLevel(
  grid: readonly AuctionGridLevel[],
  side: OrderSide,
  price: bigint,
): number | null {
  const index =
    side === "buy"
      ? grid.filter((level) => level.price <= price).length - 1
      : grid.findIndex((level) => level.price >= price);
  return index === -1 ? null : index;
}

/** What is left of `working` as an order for `grid`; null to sit this auction out. */
export function remainingOrder(
  working: WorkingLimit,
  grid: readonly AuctionGridLevel[],
  maxOrderQuantity: bigint,
  market: MarketUnits,
): BuildResult | null {
  const limitIndex = limitLevel(grid, working.side, working.limitPrice);
  if (limitIndex === null) return null;
  return buildOrder(
    {
      type: "limit",
      side: working.side,
      amount: baseUnitsToTokens(working.total - working.filled, market),
      unit: "base",
      limitIndex,
      customLevels: emptyCustomLevels(),
      maxBuy: "",
      maxSell: "",
    },
    grid,
    maxOrderQuantity,
    market,
  );
}
