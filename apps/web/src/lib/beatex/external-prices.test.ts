import { AuctionState } from "@beatex/sdk";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { BatchSummary } from "@/features/auctions/model/batch-summary";
import type { MarketConfig } from "@/lib/config/markets";

import {
  externalPrices,
  fetchExternalCandles,
  isMarketIdle,
  mergeCandles,
  sampleExternalPrices,
  toProtocolPrice,
  type ExternalCandle,
} from "./external-prices";

const tbtc: MarketConfig = {
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  address: "market",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
  binanceSymbol: "BTCUSDT",
  defaultCurveMax: "0.1",
};

function batch(blockTime: number | null, clearingVolume = 10n): BatchSummary {
  return {
    auctionId: 1n,
    state: AuctionState.Settled,
    referencePrice: 100n,
    referenceFromOracle: false,
    gridStepBps: 10,
    gridSize: 17,
    clearingIndex: 8,
    clearingPrice: 100n,
    clearingVolume,
    decryptedDemand: [],
    decryptedSupply: [],
    blockTime,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("toProtocolPrice", () => {
  it("matches the README's tBTC scale, where 85,500 means 85,500 tUSD", () => {
    expect(toProtocolPrice(85_500, tbtc)).toBe(85_500n);
  });

  it("rounds to the nearest protocol unit", () => {
    expect(toProtocolPrice(76_150.51, tbtc)).toBe(76_151n);
  });
});

describe("fetchExternalCandles", () => {
  it("reads each candle's open time and prices", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json([
        [1_000_000, "1", "2", "0.5", "76150.01", "9", 1_059_999, "x", 3],
        [1_060_000, "1", "2", "0.5", "76200.00", "9", 1_119_999, "x", 3],
      ]),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchExternalCandles(tbtc)).resolves.toEqual([
      { time: 1_000, open: 1, high: 2, low: 0.5, close: 76_150.01 },
      { time: 1_060, open: 1, high: 2, low: 0.5, close: 76_200 },
    ]);
    expect(String(fetchMock.mock.calls[0]?.at(0))).toContain("symbol=BTCUSDT");
  });

  it("asks for the candle size wanted", async () => {
    const fetchMock = vi.fn(async () => Response.json([]));
    vi.stubGlobal("fetch", fetchMock);

    await fetchExternalCandles(tbtc, { interval: 14_400 });

    const url = new URL(String(fetchMock.mock.calls[0]?.at(0)));
    expect(url.searchParams.get("interval")).toBe("4h");
  });

  it("asks for the page of candles that opened before a time", async () => {
    const fetchMock = vi.fn(async () => Response.json([]));
    vi.stubGlobal("fetch", fetchMock);

    await fetchExternalCandles(tbtc, { before: 6_000, limit: 360 });

    const url = new URL(String(fetchMock.mock.calls[0]?.at(0)));
    expect(url.searchParams.get("endTime")).toBe("5999999");
    expect(url.searchParams.get("limit")).toBe("360");
  });

  it("fetches nothing for a market with no outside pair", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      fetchExternalCandles({ ...tbtc, binanceSymbol: undefined }),
    ).resolves.toEqual([]);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("rejects on an HTTP failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("blocked", { status: 451 })),
    );

    await expect(fetchExternalCandles(tbtc)).rejects.toThrow("HTTP 451");
  });
});

describe("isMarketIdle", () => {
  const now = 10_000;

  it("is idle with no auctions, as when the indexer is down", () => {
    expect(isMarketIdle([], now)).toBe(true);
  });

  it("is idle when only pending, untimed auctions are known", () => {
    expect(isMarketIdle([batch(null)], now)).toBe(true);
  });

  it("is idle when the newest timed auction is old, as when bots stop", () => {
    expect(isMarketIdle([batch(now - 301)], now)).toBe(true);
  });

  it("is idle while auctions run but trade nothing, as with only a maker", () => {
    expect(isMarketIdle([batch(now - 20, 0n), batch(now - 900)], now)).toBe(
      true,
    );
  });

  it("is not idle once an auction trades", () => {
    expect(isMarketIdle([batch(now - 20), batch(now - 900)], now)).toBe(false);
  });
});

describe("sampleExternalPrices", () => {
  const minute: ExternalCandle = {
    time: 6_000,
    open: 76_100,
    high: 76_180,
    low: 76_020,
    close: 76_050,
  };

  it("samples a finished minute every 10 seconds, open first and close last", () => {
    const prices = sampleExternalPrices([minute], 7_000, tbtc);

    expect(prices.map((point) => point.time)).toEqual([
      6_000, 6_010, 6_020, 6_030, 6_040, 6_050,
    ]);
    expect(prices[0]?.price).toBe(76_100n);
    expect(prices.at(-1)?.price).toBe(76_050n);
  });

  it("reaches the minute's high and low and stays within them", () => {
    const values = sampleExternalPrices([minute], 7_000, tbtc).map(
      (point) => point.price,
    );

    expect(values).toContain(76_180n);
    expect(values).toContain(76_020n);
    expect(values.every((value) => value >= 76_020n && value <= 76_180n)).toBe(
      true,
    );
  });

  it("gives a minute the same prices every time", () => {
    expect(sampleExternalPrices([minute], 7_000, tbtc)).toEqual(
      sampleExternalPrices([minute], 9_000, tbtc),
    );
  });

  it("samples the running minute only up to now, ending at its latest price", () => {
    const prices = sampleExternalPrices([minute], 6_025, tbtc);

    expect(prices.map((point) => point.time)).toEqual([6_000, 6_010, 6_020]);
    expect(prices.at(-1)?.price).toBe(76_050n);
  });
});

describe("mergeCandles", () => {
  const candle = (time: number, close: number): ExternalCandle => ({
    time,
    open: close,
    high: close,
    low: close,
    close,
  });

  it("joins an older page and the latest one in time order", () => {
    expect(
      mergeCandles(
        [candle(120, 2), candle(180, 3)],
        [candle(0, 0), candle(60, 1)],
      ).map((item) => item.time),
    ).toEqual([0, 60, 120, 180]);
  });

  it("takes the newer copy of a minute both have", () => {
    expect(mergeCandles([candle(60, 1)], [candle(60, 9)])).toEqual([
      candle(60, 9),
    ]);
  });
});

describe("externalPrices", () => {
  const hour: ExternalCandle = {
    time: 36_000,
    open: 76_100,
    high: 76_400,
    low: 75_900,
    close: 76_300,
  };

  it("draws a larger candle as its open, low, high and close", () => {
    expect(externalPrices([hour], 3_600, 50_000, tbtc)).toEqual([
      { time: 36_000, price: 76_100n },
      { time: 36_900, price: 75_900n },
      { time: 37_800, price: 76_400n },
      { time: 38_700, price: 76_300n },
    ]);
  });

  it("visits the high first in a falling candle", () => {
    const falling = { ...hour, close: 76_000 };
    expect(
      externalPrices([falling], 3_600, 50_000, tbtc).map((p) => p.price),
    ).toEqual([76_100n, 76_400n, 75_900n, 76_000n]);
  });

  it("keeps the running candle's prices before now", () => {
    const prices = externalPrices([hour], 3_600, 36_400, tbtc);
    expect(prices.at(-1)?.time).toBe(36_300);
  });

  it("samples one-minute candles into auctions", () => {
    const minute = { ...hour, time: 6_000 };
    expect(externalPrices([minute], 60, 7_000, tbtc)).toHaveLength(6);
  });
});
