import { AuctionState } from "@beatex/sdk";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { BatchSummary } from "@/features/auctions/model/batch-summary";
import type { MarketConfig } from "@/lib/config/markets";

import {
  fetchExternalPrices,
  isMarketIdle,
  toProtocolPrice,
} from "./external-prices";

const tbtc: MarketConfig = {
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  address: "market",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
  binanceSymbol: "BTCUSDT",
};

function batch(blockTime: number | null): BatchSummary {
  return {
    auctionId: 1n,
    state: AuctionState.Settled,
    referencePrice: 100n,
    referenceFromOracle: false,
    gridStepBps: 10,
    gridSize: 17,
    clearingIndex: 8,
    clearingPrice: 100n,
    clearingVolume: 0n,
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

describe("fetchExternalPrices", () => {
  it("reads each candle's open time and close", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json([
        [1_000_000, "1", "2", "0.5", "76150.01", "9", 1_059_999, "x", 3],
        [1_060_000, "1", "2", "0.5", "76200.00", "9", 1_119_999, "x", 3],
      ]),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchExternalPrices(tbtc)).resolves.toEqual([
      { time: 1_000, price: 76_150n },
      { time: 1_060, price: 76_200n },
    ]);
    expect(String(fetchMock.mock.calls[0]?.at(0))).toContain("symbol=BTCUSDT");
  });

  it("fetches nothing for a market with no outside pair", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      fetchExternalPrices({ ...tbtc, binanceSymbol: undefined }),
    ).resolves.toEqual([]);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("rejects on an HTTP failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("blocked", { status: 451 })),
    );

    await expect(fetchExternalPrices(tbtc)).rejects.toThrow("HTTP 451");
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

  it("is not idle while auctions are publishing", () => {
    expect(isMarketIdle([batch(now - 20), batch(now - 900)], now)).toBe(false);
  });
});
