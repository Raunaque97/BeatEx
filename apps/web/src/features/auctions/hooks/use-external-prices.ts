"use client";

import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import type { MarketConfig } from "@/lib/config/markets";
import {
  fetchExternalPrices,
  isMarketIdle,
  type ExternalPrice,
} from "@/lib/beatex/external-prices";
import { queryKeys } from "@/lib/query/keys";

import type { BatchSummary } from "../model/batch-summary";

const NO_PRICES: readonly ExternalPrice[] = [];

/**
 * Outside prices to chart while the market is idle, so a visitor still sees a
 * price when the indexer or the bots are down. Empty whenever auctions are
 * running, and for markets with no outside pair.
 */
export function useExternalPrices(
  market: MarketConfig,
  history: readonly BatchSummary[],
  historyLoading: boolean,
): readonly ExternalPrice[] {
  const [now, setNow] = useState(() => Date.now() / 1_000);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now() / 1_000), 30_000);
    return () => clearInterval(timer);
  }, []);

  const idle = !historyLoading && isMarketIdle(history, now);
  const prices = useQuery({
    queryKey: queryKeys.externalPrices(market.address),
    enabled: idle && market.binanceSymbol !== undefined,
    refetchInterval: 30_000,
    queryFn: ({ signal }) => fetchExternalPrices(market, signal),
  });
  return idle ? (prices.data ?? NO_PRICES) : NO_PRICES;
}
