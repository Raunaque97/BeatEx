"use client";

import type { MarketAccount } from "@beatex/sdk";
import { useInfiniteQuery } from "@tanstack/react-query";
import { useState } from "react";

import { fetchAuctionHistory, toBatchSummary } from "@/lib/beatex/indexer";
import { queryKeys } from "@/lib/query/keys";

import type { BatchSummary } from "../model/batch-summary";

/** The indexer's page cap. */
const PAGE = 500;

/**
 * Older finished auctions, one indexer page at a time, loaded only when the
 * chart is zoomed or panned past what it already has. Finished auctions never
 * change, so these pages are not polled.
 */
export function useOlderAuctions(
  market: string,
  marketAccount: MarketAccount | undefined,
  /** The oldest auction already loaded; older pages start below it. */
  before: bigint | null,
) {
  // Anchor once: the recent window's oldest auction advances every few
  // seconds, and older pages must not restart each time it does.
  const [anchor, setAnchor] = useState<{
    readonly market: string;
    readonly before: bigint;
  } | null>(null);
  if (before !== null && (anchor === null || anchor.market !== market)) {
    setAnchor({ market, before });
  }
  const start = anchor?.market === market ? anchor.before : null;

  const query = useInfiniteQuery({
    queryKey: [...queryKeys.auctionHistory(market), "older", start?.toString()],
    enabled: false,
    initialPageParam: start,
    staleTime: Infinity,
    queryFn: async ({ pageParam, signal }) => {
      const records = await fetchAuctionHistory(
        market,
        { limit: PAGE, ...(pageParam === null ? {} : { before: pageParam }) },
        signal,
      );
      return records.map((record) =>
        toBatchSummary(record, {
          gridStepBps: marketAccount!.gridStepBps,
          gridSize: marketAccount!.gridRadius * 2 + 1,
        }),
      );
    },
    getNextPageParam: (lastPage) =>
      lastPage.length < PAGE ? undefined : lastPage.at(-1)!.auctionId,
  });

  const batches: BatchSummary[] = query.data?.pages.flat() ?? [];
  const loadOlder = () => {
    if (start === null || marketAccount === undefined) return;
    if (query.isFetching) return;
    if (query.data === undefined) void query.refetch();
    else if (query.hasNextPage) void query.fetchNextPage();
  };
  return {
    batches,
    loadOlder,
    loading: query.isFetching,
    exhausted: query.data !== undefined && !query.hasNextPage,
  };
}
