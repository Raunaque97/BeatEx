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
/** The newest candle's close moves live, so this is how often the price ticks. */
const REFRESH_MS = 10_000;

/**
 * Outside prices to chart while the market is idle, so a visitor still sees a
 * moving price when nothing trades. Empty once auctions trade, and for markets
 * with no outside pair.
 */
export function useExternalPrices(
  market: MarketConfig,
  history: readonly BatchSummary[],
  historyLoading: boolean,
): readonly ExternalPrice[] {
  const [now, setNow] = useState(() => Date.now() / 1_000);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now() / 1_000), REFRESH_MS);
    return () => clearInterval(timer);
  }, []);

  const idle = !historyLoading && isMarketIdle(history, now);
  const prices = useQuery({
    queryKey: queryKeys.externalPrices(market.address),
    enabled: idle && market.binanceSymbol !== undefined,
    refetchInterval: REFRESH_MS,
    queryFn: ({ signal }) => fetchExternalPrices(market, signal),
  });
  return idle ? (prices.data ?? NO_PRICES) : NO_PRICES;
}
