import type { ExternalPrice } from "@/lib/beatex/external-prices";

import type { BatchSummary } from "./batch-summary";
import type { ChartPoint, PointSource } from "./chart-series";

/** A chart point placed in time (unix seconds). */
export interface TimedPoint extends ChartPoint {
  readonly time: number;
}

/** What the chart draws: an auction, or an outside price standing in for one. */
export interface SeriesPoint {
  readonly key: string;
  readonly time: number;
  readonly price: bigint;
  readonly volume: bigint;
  /** `outside` for a sampled outside price, with no auction behind it. */
  readonly source: PointSource | "outside";
  readonly auction: BatchSummary | null;
}

/**
 * Auctions merged with outside prices, oldest first, so the chart moves
 * between trades. Over the outside prices' span they replace every auction
 * that did not trade, whose reference price they stand in for. Auctions that
 * traded always stay, and outside prices within `near` seconds of one give way
 * to it, so a busy market charts only its own trades.
 */
export function withOutsidePrices(
  auctions: readonly TimedPoint[],
  outside: readonly ExternalPrice[],
  near: number,
): SeriesPoint[] {
  const from = outside[0]?.time ?? Infinity;
  const trades = auctions
    .filter((point) => point.source === "clearing")
    .map((point) => point.time);
  // Both lists run oldest first, so one pass finds each price's nearest trade.
  let next = 0;
  const gaps = outside.filter((point) => {
    while (next < trades.length && trades[next]! < point.time) next += 1;
    const after = trades[next];
    const before = trades[next - 1];
    return !(
      (after !== undefined && after - point.time < near) ||
      (before !== undefined && point.time - before < near)
    );
  });
  return [
    ...auctions
      .filter((point) => point.source === "clearing" || point.time < from)
      .map((point): SeriesPoint => ({
        key: point.auctionId.toString(),
        time: point.time,
        price: point.price,
        volume: point.volume,
        source: point.source,
        auction: point.auction,
      })),
    ...gaps.map((point): SeriesPoint => ({
      key: `outside-${point.time}`,
      time: point.time,
      price: point.price,
      volume: 0n,
      source: "outside",
      auction: null,
    })),
  ].sort((left, right) => left.time - right.time);
}

/** Auction length to assume before any two timed auctions reveal the real one. */
const FALLBACK_SECONDS_PER_AUCTION = 10;

/**
 * Seconds per auction, from the auctions whose publication time is known:
 * the median over neighbouring timed pairs, so one slow publication can't skew it.
 */
export function secondsPerAuction(points: readonly ChartPoint[]): number {
  const timed = points.filter(
    (point) => typeof point.auction.blockTime === "number",
  );
  const rates: number[] = [];
  for (let index = 1; index < timed.length; index += 1) {
    const previous = timed[index - 1]!;
    const current = timed[index]!;
    const auctions = Number(current.auctionId - previous.auctionId);
    if (auctions <= 0) continue;
    rates.push(
      (current.auction.blockTime! - previous.auction.blockTime!) / auctions,
    );
  }
  if (rates.length === 0) return FALLBACK_SECONDS_PER_AUCTION;
  rates.sort((a, b) => a - b);
  const median = rates[Math.floor(rates.length / 2)]!;
  return median > 0 ? median : FALLBACK_SECONDS_PER_AUCTION;
}

/**
 * Place every point in time. Points the indexer timed keep their block time;
 * the rest (still pending, or published without one) are placed from the
 * nearest timed auction at the observed auction rate, or from `now` for the
 * live auction when nothing is timed yet.
 */
export function placeInTime(
  points: readonly ChartPoint[],
  live: { readonly auctionId: bigint; readonly now: number } | null,
): TimedPoint[] {
  const rate = secondsPerAuction(points);
  const anchors = points.filter(
    (point) => typeof point.auction.blockTime === "number",
  );
  const anchorFor = (id: bigint) => {
    let best: { id: bigint; time: number } | null =
      live === null ? null : { id: live.auctionId, time: live.now };
    for (const point of anchors) {
      if (
        best === null ||
        absolute(point.auctionId - id) < absolute(best.id - id)
      ) {
        best = { id: point.auctionId, time: point.auction.blockTime! };
      }
    }
    return best;
  };
  let previous = -Infinity;
  return points.map((point) => {
    let time: number;
    if (typeof point.auction.blockTime === "number") {
      time = point.auction.blockTime;
    } else {
      const anchor = anchorFor(point.auctionId);
      time =
        anchor === null
          ? 0
          : anchor.time + Number(point.auctionId - anchor.id) * rate;
    }
    // Publication times jitter; keep auctions in order so the line never
    // doubles back and each one lands in the right candle.
    time = Math.max(time, previous);
    previous = time;
    return { ...point, time };
  });
}

function absolute(value: bigint): bigint {
  return value < 0n ? -value : value;
}

export interface Candle {
  /** Bucket start, unix seconds. */
  readonly time: number;
  readonly open: bigint;
  readonly high: bigint;
  readonly low: bigint;
  readonly close: bigint;
  readonly volume: bigint;
  /** Auctions in the bucket. */
  readonly count: number;
}

/** Group points into candles of `seconds` each (1-minute by default), oldest first. */
export function toCandles(
  points: readonly Pick<SeriesPoint, "time" | "price" | "volume">[],
  seconds = 60,
): Candle[] {
  const buckets = new Map<number, Candle>();
  for (const point of points) {
    const time = Math.floor(point.time / seconds) * seconds;
    const current = buckets.get(time);
    buckets.set(
      time,
      current === undefined
        ? {
            time,
            open: point.price,
            high: point.price,
            low: point.price,
            close: point.price,
            volume: point.volume,
            count: 1,
          }
        : {
            ...current,
            high: point.price > current.high ? point.price : current.high,
            low: point.price < current.low ? point.price : current.low,
            close: point.price,
            volume: current.volume + point.volume,
            count: current.count + 1,
          },
    );
  }
  return [...buckets.values()].sort((left, right) => left.time - right.time);
}

const DAY = 86_400;

/** Tick spacings for the time axis, in seconds. */
const TICK_STEPS = [
  15,
  30,
  60,
  120,
  300,
  600,
  900,
  1_800,
  3_600,
  7_200,
  14_400,
  21_600,
  43_200,
  DAY,
  2 * DAY,
  7 * DAY,
  14 * DAY,
];

const MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];

/**
 * Round-numbered time-axis ticks across [start, end], at most `max` of them,
 * on the local clock: `utcOffset` is seconds ahead of UTC, so hours and days
 * start where the viewer's do rather than at UTC's.
 */
export function timeTicks(
  start: number,
  end: number,
  max = 7,
  utcOffset = localUtcOffset(start),
): number[] {
  const span = Math.max(1, end - start);
  const step =
    TICK_STEPS.find((candidate) => span / candidate <= max) ??
    TICK_STEPS.at(-1)!;
  const ticks: number[] = [];
  for (
    let tick = Math.ceil((start + utcOffset) / step) * step - utcOffset;
    tick <= end;
    tick += step
  ) {
    ticks.push(tick);
  }
  return ticks;
}

function localUtcOffset(time: number): number {
  return -new Date(time * 1_000).getTimezoneOffset() * 60;
}

/**
 * A tick label in local time: the date for ticks a day or more apart, and
 * seconds only when they are under a minute apart.
 */
export function formatTick(time: number, step: number): string {
  const date = new Date(time * 1_000);
  if (step >= DAY) return `${MONTHS[date.getMonth()]} ${date.getDate()}`;
  const pad = (value: number) => value.toString().padStart(2, "0");
  const clock = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  return step < 60 ? `${clock}:${pad(date.getSeconds())}` : clock;
}
