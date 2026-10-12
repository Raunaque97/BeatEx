"use client";

import {
  auctionIdAtSlot,
  decodeAnyAuction,
  deriveAuctionPda,
  isAuctionResult,
} from "@beatex/sdk";
import { address } from "@solana/kit";
import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";

import { useMarketAccount } from "@/features/markets/hooks/use-market-account";
import { beatexRpc } from "@/lib/beatex/rpc";
import { queryKeys } from "@/lib/query/keys";

import { describeCurrentBatch } from "../model/current-batch";
import { useAuctionHistory } from "./use-auction-history";
import { useOraclePrice } from "./use-oracle-price";
import { useSlotClock } from "./use-slot-clock";

/** Everything the trading screen reads from chain, composed in one place. */
export function useMarketSnapshot(market: string) {
  const { slot, timing: slotTiming } = useSlotClock();
  const marketQuery = useMarketAccount(market);
  const marketAccount = marketQuery.data;
  const oracle = useOraclePrice(market, marketAccount);

  const auctionId =
    marketAccount === undefined || slot === null
      ? null
      : auctionIdAtSlot(marketAccount, slot);

  const currentAuction = useQuery({
    queryKey: queryKeys.auction(market, auctionId ?? -1n),
    enabled: auctionId !== null,
    refetchInterval: 4_000,
    queryFn: async () => {
      const [auctionAddress] = await deriveAuctionPda(
        address(market),
        auctionId!,
      );
      const account = await beatexRpc.getAccount(auctionAddress);
      if (account === null) return null;
      const auction = decodeAnyAuction(account.data);
      // A finished auction is compacted and no longer the open batch.
      return isAuctionResult(auction) ? null : auction;
    },
  });

  const history = useAuctionHistory(market, marketAccount, auctionId);

  const batch = useMemo(() => {
    if (marketAccount === undefined || slot === null) return null;
    // While a new batch's account is loading, show the slot-derived clock and the
    // preview grid rather than blanking the header at every batch boundary.
    return describeCurrentBatch(
      marketAccount,
      slot,
      currentAuction.data ?? null,
      oracle.data ?? null,
    );
  }, [marketAccount, slot, currentAuction.data, oracle.data]);

  return {
    slot,
    slotTiming,
    market: marketAccount,
    // Keep showing the last good data through transient RPC errors (e.g. 429s).
    marketError: marketAccount === undefined ? marketQuery.error : null,
    oracle: oracle.data ?? null,
    batch,
    history: history.data ?? [],
    historyLoading: history.isPending,
  };
}

export type MarketSnapshot = ReturnType<typeof useMarketSnapshot>;
