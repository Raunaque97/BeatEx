"use client";

import {
  keepPreviousData,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";

import type { MarketConfig } from "@/lib/config/markets";
import {
  externalPrices,
  fetchExternalCandles,
  isMarketIdle,
  mergeCandles,
  OLDER_KLINE_LIMIT,
  type ExternalCandle,
  type ExternalPrice,
} from "@/lib/beatex/external-prices";
import { queryKeys } from "@/lib/query/keys";

import type { BatchSummary } from "../model/batch-summary";

const NO_PRICES: readonly ExternalPrice[] = [];
/** The newest candle's close moves live, so this is how often the price ticks. */
const REFRESH_MS = 10_000;

/**
 * Outside prices, sampled about once per auction, to chart while the market is
 * idle, so a visitor still sees a moving price when nothing trades. Empty
 * once auctions trade, and for markets with no outside pair. They come from
 * `interval`-second candles, the size the chart is drawing, and `loadOlder`
 * extends them back a page at a time as the chart zooms or pans out.
 */
export function useExternalPrices(
  market: MarketConfig,
  history: readonly BatchSummary[],
  historyLoading: boolean,
  interval: number,
): {
  readonly prices: readonly ExternalPrice[];
  readonly loadOlder: () => void;
} {
  const [now, setNow] = useState(() => Date.now() / 1_000);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now() / 1_000), REFRESH_MS);
    return () => clearInterval(timer);
  }, []);

  const queryClient = useQueryClient();
  const queryKey = queryKeys.externalPrices(market.address, interval);
  const idle = !historyLoading && isMarketIdle(history, now);
  // One growing list: each refresh merges the latest candles into the older
  // pages already loaded, so the moving live window never leaves a gap.
  const candles = useQuery({
    queryKey,
    enabled: idle && market.binanceSymbol !== undefined,
    refetchInterval: REFRESH_MS,
    // Keep drawing the last size's candles while a new size loads.
    placeholderData: keepPreviousData,
    queryFn: async ({ signal }) => {
      const latest = await fetchExternalCandles(market, { interval, signal });
      return mergeCandles(
        queryClient.getQueryData<readonly ExternalCandle[]>(queryKey) ?? [],
        latest,
      );
    },
  });
  // A placeholder holds the previous size's candles.
  const shownInterval = candles.isPlaceholderData ? null : interval;
  const [lastInterval, setLastInterval] = useState(interval);
  if (shownInterval !== null && shownInterval !== lastInterval) {
    setLastInterval(shownInterval);
  }
  const dataInterval = shownInterval ?? lastInterval;
  const sampled = useMemo(
    () =>
      candles.data === undefined
        ? NO_PRICES
        : externalPrices(candles.data, dataInterval, now, market),
    [candles.data, dataInterval, now, market],
  );

  // The chart asks on every frame near the edge; one request per oldest
  // candle at a time, and again only after a pause if it failed or was empty.
  const lastAttempt = useRef<{
    interval: number;
    before: number;
    at: number;
  } | null>(null);
  const loadOlder = () => {
    const before = candles.data?.[0]?.time;
    if (!idle || candles.isPlaceholderData || before === undefined) return;
    const attempt = lastAttempt.current;
    if (
      attempt?.interval === interval &&
      attempt.before === before &&
      Date.now() - attempt.at < REFRESH_MS
    ) {
      return;
    }
    lastAttempt.current = { interval, before, at: Date.now() };
    fetchExternalCandles(market, {
      interval,
      before,
      limit: OLDER_KLINE_LIMIT,
    })
      .then((page) =>
        queryClient.setQueryData<readonly ExternalCandle[]>(
          queryKey,
          (previous = []) => mergeCandles(page, previous),
        ),
      )
      .catch(() => undefined);
  };

  return { prices: idle ? sampled : NO_PRICES, loadOlder };
}
