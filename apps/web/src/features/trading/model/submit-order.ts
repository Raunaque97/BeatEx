import {
  BeatExError,
  type BeatExClient,
  type ProofExecutionOptions,
} from "@beatex/sdk";

import type { CurrentBatch } from "@/features/auctions/model/current-batch";
import type { BuiltOrder } from "@/features/orders/model/order-curve";
import { requestMaker } from "@/lib/beatex/maker";
import type { MarketConfig } from "@/lib/config/markets";

/**
 * Stop taking orders this many slots before an auction closes. Building,
 * proving, and sending an order takes 3–4 s end to end, and Devnet's slots can
 * run as fast as 0.24 s.
 */
export const CLOSING_SLOTS = 20n;
const AUCTION_WAIT_MS = 10_000;
const AUCTION_POLL_MS = 400;

const GRID_CHANGED =
  "The auction price grid changed. Review your order and try again.";

class AuctionMovedOnError extends Error {}

/**
 * Whether `error` means the auction closed while the order was on its way: the
 * next one wasn't open yet, it failed the deadline check in simulation, or it
 * landed on-chain just too late.
 */
export function missedAuction(error: unknown): boolean {
  return (
    error instanceof AuctionMovedOnError ||
    (error instanceof BeatExError && error.code === "TRANSACTION_FAILED") ||
    (error instanceof Error &&
      /QueueInvariant|auction is not initialized/.test(error.message))
  );
}

/** Whether `batch` can take an order now, opened yet or not. */
export function isAcceptingOrders(batch: CurrentBatch): boolean {
  return (
    batch.grid !== null &&
    (batch.phase === "collecting" || batch.gridIsPreview) &&
    batch.slotsLeft >= CLOSING_SLOTS
  );
}

/**
 * The server opens each auction a trader needs. Wait for it rather than creating
 * it here: racing it fails, and a winning trader would pay the auction's
 * ~0.0376 SOL of rent.
 */
async function waitForAuction(
  client: BeatExClient,
  market: string,
  auctionId: bigint,
) {
  const deadline = Date.now() + AUCTION_WAIT_MS;
  for (;;) {
    const auction = await client.getAuction(market, auctionId);
    if (auction !== null) return auction;
    if (Date.now() >= deadline)
      throw new Error("This auction is still opening. Try again in a moment.");
    await new Promise((resolve) => setTimeout(resolve, AUCTION_POLL_MS));
  }
}

/** Place `order` into the running auction, replacing `replacing` if there is one. */
export async function submitOrder({
  client,
  market,
  order,
  batch,
  replacing,
  proofOptions,
}: {
  readonly client: BeatExClient;
  readonly market: MarketConfig;
  readonly order: BuiltOrder;
  readonly batch: CurrentBatch;
  /** The auction of the order this one replaces. */
  readonly replacing: bigint | null;
  readonly proofOptions: ProofExecutionOptions;
}): Promise<bigint> {
  if (batch.auction === null) {
    await requestMaker(market.slug, client.owner, { open: true });
  }
  // Pin the displayed grid to the auction selected for submission. A
  // boundary or a newly snapshotted reference requires another review.
  const auctionId = await client.getCurrentAuctionId(market.address);
  if (auctionId !== batch.auctionId)
    throw new AuctionMovedOnError(GRID_CHANGED);
  const auction = await waitForAuction(client, market.address, auctionId);
  if (
    batch.auction !== null &&
    (auction.referencePrice !== batch.referencePrice ||
      auction.gridStepBps !== batch.auction.gridStepBps)
  ) {
    throw new Error(GRID_CHANGED);
  }
  const params = {
    market: market.address,
    curve: order.curve,
    proofOptions,
  };
  if (replacing !== null) {
    if (replacing !== auctionId)
      throw new Error(
        "Your previous order must settle before placing a new one.",
      );
    await client.replaceOrder(params);
  } else {
    await client.placeOrder({ ...params, initializeAuction: false });
  }
  return auctionId;
}
