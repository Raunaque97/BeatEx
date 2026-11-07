import { AuctionState, BASE_UNIT_SIZE } from "@beatex/sdk";
import { z } from "zod";

import type { BatchSummary } from "@/features/auctions/model/batch-summary";
import type { MarketConfig } from "@/lib/config/markets";

/** Binance's public market-data host: no API key, and it allows browser requests. */
const BINANCE_KLINES = "https://data-api.binance.vision/api/v3/klines";
/** Latest candles to request: two hours of minutes, past the chart's default span. */
const KLINE_LIMIT = 120;
/** Older candles to request at a time as the chart zooms or pans out. */
export const OLDER_KLINE_LIMIT = 500;

/** Binance's name for each candle size the chart draws, in seconds. */
const KLINE_INTERVALS: ReadonlyMap<number, string> = new Map([
  [60, "1m"],
  [300, "5m"],
  [900, "15m"],
  [3_600, "1h"],
  [14_400, "4h"],
  [86_400, "1d"],
]);
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

/** One outside candle, in quote tokens per base token. */
export interface ExternalCandle {
  /** Open time, unix seconds. */
  readonly time: number;
  readonly open: number;
  readonly high: number;
  readonly low: number;
  readonly close: number;
}

/** Seconds a sampled candle covers: one minute. */
const CANDLE_SECONDS = 60;
/** Spacing of sampled prices: about one auction each. */
const SAMPLE_SECONDS = 10;

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

/**
 * Candles for `market`, oldest first, `interval` seconds each (one minute by
 * default): the latest ones, or with `before` (unix seconds) the ones that
 * opened before it.
 */
export async function fetchExternalCandles(
  market: MarketConfig,
  options: {
    readonly interval?: number;
    readonly before?: number;
    readonly limit?: number;
    readonly signal?: AbortSignal;
  } = {},
): Promise<readonly ExternalCandle[]> {
  if (market.binanceSymbol === undefined) return [];
  const params = new URLSearchParams({
    symbol: market.binanceSymbol,
    interval: klineInterval(options.interval ?? CANDLE_SECONDS),
    limit: String(options.limit ?? KLINE_LIMIT),
  });
  if (options.before !== undefined) {
    params.set("endTime", String(options.before * 1_000 - 1));
  }
  const response = await fetch(`${BINANCE_KLINES}?${params}`, {
    signal: options.signal,
    credentials: "omit",
  });
  if (!response.ok) {
    throw new Error(`Binance returned HTTP ${response.status}`);
  }
  return klinesSchema.parse(await response.json()).map((row) => ({
    time: row[0] / 1_000,
    open: Number(row[1]),
    high: Number(row[2]),
    low: Number(row[3]),
    close: Number(row[4]),
  }));
}

function klineInterval(seconds: number): string {
  const name = KLINE_INTERVALS.get(seconds);
  if (name === undefined) throw new Error(`no ${seconds}-second candles`);
  return name;
}

/** Two candle lists as one, oldest first; `next` wins for a minute in both. */
export function mergeCandles(
  previous: readonly ExternalCandle[],
  next: readonly ExternalCandle[],
): readonly ExternalCandle[] {
  const byTime = new Map<number, ExternalCandle>();
  for (const candle of [...previous, ...next]) byTime.set(candle.time, candle);
  return [...byTime.values()].sort((left, right) => left.time - right.time);
}

/**
 * Prices to chart from `interval`-second candles. One-minute candles are
 * sampled about once per auction; larger ones, for views too wide to show
 * auctions, become their open, high, low and close in the order a price
 * would most simply take, which the chart's candles of that size match.
 */
export function externalPrices(
  candles: readonly ExternalCandle[],
  interval: number,
  now: number,
  market: Pick<MarketConfig, "base" | "quote">,
): readonly ExternalPrice[] {
  if (interval === CANDLE_SECONDS) {
    return sampleExternalPrices(candles, now, market);
  }
  return candles.flatMap((candle) => {
    // The running candle has only lasted until now.
    const step = Math.max(0, Math.min(interval, now - candle.time)) / 4;
    const path =
      candle.close >= candle.open
        ? [candle.open, candle.low, candle.high, candle.close]
        : [candle.open, candle.high, candle.low, candle.close];
    return path.map((value, index) => ({
      time: candle.time + index * step,
      price: toProtocolPrice(value, market),
    }));
  });
}

/**
 * Prices every `SAMPLE_SECONDS`, so an idle market charts like one running
 * auctions. Each minute opens at its open and ends at its close; the prices
 * between are random within its range and include its high and low, so the
 * chart's own one-minute candles match the outside ones. The randomness is
 * seeded by the minute, so refetching never reshuffles the past. The running
 * minute is sampled only up to `now`, ending at its latest price.
 */
export function sampleExternalPrices(
  candles: readonly ExternalCandle[],
  now: number,
  market: Pick<MarketConfig, "base" | "quote">,
): readonly ExternalPrice[] {
  const prices: ExternalPrice[] = [];
  for (const candle of candles) {
    const count = Math.min(
      CANDLE_SECONDS / SAMPLE_SECONDS,
      Math.floor((now - candle.time) / SAMPLE_SECONDS) + 1,
    );
    sampleCandle(candle, count).forEach((value, index) =>
      prices.push({
        time: candle.time + index * SAMPLE_SECONDS,
        price: toProtocolPrice(value, market),
      }),
    );
  }
  return prices;
}

/** `count` prices through `candle`: open first, close last, high and low between. */
function sampleCandle(candle: ExternalCandle, count: number): number[] {
  if (count <= 0) return [];
  if (count === 1) return [candle.close];
  const random = seededRandom(candle.time);
  const between = Array.from(
    { length: count - 2 },
    () => candle.low + random() * (candle.high - candle.low),
  );
  // Put the extremes at two random slots, when there are two.
  if (between.length >= 2) {
    const high = Math.floor(random() * between.length);
    const low =
      (high + 1 + Math.floor(random() * (between.length - 1))) % between.length;
    between[high] = candle.high;
    between[low] = candle.low;
  }
  return [candle.open, ...between, candle.close];
}

/** A small deterministic generator (mulberry32) in [0, 1). */
function seededRandom(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4_294_967_296;
  };
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
