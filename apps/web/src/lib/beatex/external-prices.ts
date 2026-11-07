import { AuctionState, BASE_UNIT_SIZE } from "@beatex/sdk";
import { z } from "zod";

import type { BatchSummary } from "@/features/auctions/model/batch-summary";
import type { MarketConfig } from "@/lib/config/markets";

/** Binance's public market-data host: no API key, and it allows browser requests. */
const BINANCE_KLINES = "https://data-api.binance.vision/api/v3/klines";
/** One-minute candles to request: two hours, past the chart's default span. */
const KLINE_LIMIT = 120;
/** With no trade for this long, the market reads as idle. */
const IDLE_AFTER_SECONDS = 5 * 60;

/** A row is [openTime, open, high, low, close, ...]; only these fields are read. */
const klinesSchema = z.array(
  z
    .tuple([z.number(), z.string(), z.string(), z.string(), z.string()])
    .rest(z.unknown()),
);

/** An outside market price, never an auction: it did not clear on BeatEx. */
export interface ExternalPrice {
  /** Unix seconds. */
  readonly time: number;
  /** In protocol price units, like `BatchSummary.clearingPrice`. */
  readonly price: bigint;
}

/** Convert a price in quote tokens per base token to protocol price units. */
export function toProtocolPrice(
  quotePerBase: number,
  market: Pick<MarketConfig, "base" | "quote">,
): bigint {
  return BigInt(
    Math.round(
      (quotePerBase * 10 ** market.quote.decimals * Number(BASE_UNIT_SIZE)) /
        10 ** market.base.decimals,
    ),
  );
}

/** Recent one-minute closes for `market`, oldest first. */
export async function fetchExternalPrices(
  market: MarketConfig,
  signal?: AbortSignal,
): Promise<readonly ExternalPrice[]> {
  if (market.binanceSymbol === undefined) return [];
  const params = new URLSearchParams({
    symbol: market.binanceSymbol,
    interval: "1m",
    limit: String(KLINE_LIMIT),
  });
  const response = await fetch(`${BINANCE_KLINES}?${params}`, {
    signal,
    credentials: "omit",
  });
  if (!response.ok) {
    throw new Error(`Binance returned HTTP ${response.status}`);
  }
  return klinesSchema.parse(await response.json()).map((row) => ({
    time: row[0] / 1_000,
    price: toProtocolPrice(Number(row[4]), market),
  }));
}

/**
 * Whether no auction traded within the idle window. Pending auctions have no
 * block time, so this holds when the indexer is down, when the bots have
 * stopped, and when auctions run with only a maker's quote: those clear no
 * volume and repeat the last clearing price, however old it is.
 */
export function isMarketIdle(
  history: readonly BatchSummary[],
  now: number,
): boolean {
  return !history.some(
    (batch) =>
      batch.state === AuctionState.Settled &&
      batch.clearingVolume > 0n &&
      typeof batch.blockTime === "number" &&
      now - batch.blockTime < IDLE_AFTER_SECONDS,
  );
}
