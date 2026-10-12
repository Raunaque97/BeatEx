import { publicEnv } from "@/lib/config/public-env";

const root = ["beatex", publicEnv.cluster, publicEnv.programId] as const;

/** Query keys are scoped by cluster, program, and market so cached data never crosses them. */
export const queryKeys = {
  slot: () => [...root, "slot"] as const,
  market: (market: string) => [...root, market, "market"] as const,
  oracle: (market: string) => [...root, market, "oracle"] as const,
  auction: (market: string, auctionId: bigint) =>
    [...root, market, "auction", auctionId.toString()] as const,
  auctionHistory: (market: string) =>
    [...root, market, "auction-history"] as const,
  /** Public wallet balances only; private balances never enter the query cache. */
  walletBalances: (owner: string) => [...root, "wallet", owner] as const,
  /** The owner's public transaction list; private amounts are never in it. */
  walletHistory: (owner: string) =>
    [...root, "wallet", owner, "history"] as const,
};
