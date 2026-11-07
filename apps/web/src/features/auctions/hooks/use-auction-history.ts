"use client";

import {
  AuctionState,
  decodeAnyAuction,
  deriveAuctionPda,
  isAuctionResult,
  type MarketAccount,
} from "@beatex/sdk";
import { address } from "@solana/kit";
import { useQuery } from "@tanstack/react-query";
import { useMemo } from "react";

import { beatexRpc } from "@/lib/beatex/rpc";
import { fetchIndexedAuctions } from "@/lib/beatex/indexer";
import { queryKeys } from "@/lib/query/keys";

import type { BatchSummary } from "../model/batch-summary";

const MAX_ACCOUNTS_PER_CALL = 100;
/** Batches whose publication may still land after they stop collecting. */
const IN_FLIGHT_BATCHES = 24n;

/**
 * The still-pending auctions among `ids`. Finished auctions are compacted on
 * chain and come from the indexer instead.
 */
async function fetchPendingAuctions(
  market: string,
  ids: readonly bigint[],
): Promise<BatchSummary[]> {
  const addresses = await Promise.all(
    ids.map(async (id) => (await deriveAuctionPda(address(market), id))[0]),
  );
  const pending: BatchSummary[] = [];
  for (
    let start = 0;
    start < addresses.length;
    start += MAX_ACCOUNTS_PER_CALL
  ) {
    const accounts = await beatexRpc.getMultipleAccounts(
      addresses.slice(start, start + MAX_ACCOUNTS_PER_CALL),
    );
    for (const account of accounts) {
      if (account === null) continue;
      const auction = decodeAnyAuction(account.data);
      if (!isAuctionResult(auction) && auction.state === AuctionState.Pending) {
        pending.push(auction);
      }
    }
  }
  return pending;
}

/**
 * The most recent `limit` batches: finished ones from the auction-history
 * indexer, plus in-flight ones read directly from their pending accounts.
 * The two load separately, so the on-chain batches survive an indexer outage
 * and the last good indexer page survives a failed refetch.
 */
export function useAuctionHistory(
  market: string,
  marketAccount: MarketAccount | undefined,
  currentAuctionId: bigint | null,
  limit = 60,
) {
  const finished = useQuery({
    queryKey: [...queryKeys.auctionHistory(market), "finished", limit],
    enabled: marketAccount !== undefined,
    retry: 1,
    refetchInterval: 8_000,
    queryFn: () =>
      fetchIndexedAuctions(
        market,
        {
          gridStepBps: marketAccount!.gridStepBps,
          gridSize: marketAccount!.gridRadius * 2 + 1,
        },
        limit,
      ),
  });

  const pending = useQuery({
    queryKey: [
      ...queryKeys.auctionHistory(market),
      "pending",
      currentAuctionId?.toString(),
    ],
    enabled: currentAuctionId !== null && marketAccount !== undefined,
    placeholderData: (previous) => previous,
    refetchInterval: 8_000,
    queryFn: () => {
      const inFlight: bigint[] = [];
      for (
        let id = currentAuctionId! - IN_FLIGHT_BATCHES;
        id <= currentAuctionId!;
        id += 1n
      ) {
        if (id >= 0n) inFlight.push(id);
      }
      return fetchPendingAuctions(market, inFlight);
    },
  });

  const data = useMemo(() => {
    const byId = new Map<bigint, BatchSummary>();
    for (const batch of [...(pending.data ?? []), ...(finished.data ?? [])])
      byId.set(batch.auctionId, batch);
    return [...byId.values()]
      .sort((a, b) => (a.auctionId < b.auctionId ? 1 : -1))
      .slice(0, limit);
  }, [pending.data, finished.data, limit]);

  // A query that never succeeded reads as pending again on every refetch.
  return {
    data,
    isPending: finished.isPending && finished.errorUpdateCount === 0,
  };
}
