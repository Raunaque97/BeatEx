import type { AuctionAccount } from "@beatex/sdk";

/**
 * What the price chart and batch reveal need about one batch. Pending batches
 * come straight from their on-chain `Auction` account; finished batches come
 * from the indexer, because the program compacts their accounts.
 */
export type BatchSummary = Pick<
  AuctionAccount,
  | "auctionId"
  | "state"
  | "referencePrice"
  | "referenceFromOracle"
  | "gridStepBps"
  | "gridSize"
  | "clearingIndex"
  | "clearingPrice"
  | "clearingVolume"
  | "decryptedDemand"
  | "decryptedSupply"
> & {
  /** Unix seconds of the auction's publication, when the indexer knows it. */
  readonly blockTime?: number | null;
};
