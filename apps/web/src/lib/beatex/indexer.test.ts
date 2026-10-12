import { AuctionState } from "@beatex/sdk";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchAuctionHistory, toBatchSummary } from "./indexer";

const settled = {
  market: "3gnpAdrTnHxgQMiGgR7y6zPd2vP4uZLqyWt1TvVtCdGt",
  auctionId: "420",
  outcome: "settled",
  referencePrice: "83591",
  referenceFromOracle: false,
  gridStepBps: 10,
  gridSize: 17,
  clearingIndex: 8,
  clearingPrice: "83591",
  clearingVolume: "4000",
  acceptedOrderCount: "2",
  demand: Array(17).fill("1000"),
  supply: Array(17).fill("2000"),
  slot: "510057531",
  signature: "sig",
  blockTime: 1791758817,
};

afterEach(() => vi.unstubAllGlobals());

describe("fetchAuctionHistory", () => {
  it("parses indexer records with u64 strings as bigints", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json([
        settled,
        {
          ...settled,
          auctionId: "419",
          outcome: "empty",
          demand: undefined,
          supply: undefined,
        },
      ]),
    );
    vi.stubGlobal("fetch", fetchMock);
    const records = await fetchAuctionHistory(settled.market, {
      limit: 2,
      before: 421n,
    });
    const [requestUrl, init] = fetchMock.mock.calls[0] as unknown[];
    expect(String(requestUrl).startsWith("/api/indexer/")).toBe(true);
    const url = new URL(String(requestUrl), "https://beatex.example");
    expect(url.pathname).toBe(
      `/api/indexer/v1/markets/${settled.market}/auctions`,
    );
    expect(init).toMatchObject({ credentials: "omit" });
    expect(init).not.toHaveProperty("cache", "no-store");
    expect(url.searchParams.get("limit")).toBe("2");
    expect(url.searchParams.get("before")).toBe("421");
    expect(records[0]).toMatchObject({
      auctionId: 420n,
      clearingVolume: 4000n,
      outcome: "settled",
    });
    expect(records[0]!.demand![0]).toBe(1000n);
    expect(records[1]).toMatchObject({ auctionId: 419n, outcome: "empty" });
  });

  it("rejects HTTP errors and malformed records", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response("down", { status: 503 })),
    );
    await expect(fetchAuctionHistory("m")).rejects.toThrow("HTTP 503");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => Response.json([{ ...settled, auctionId: -1 }])),
    );
    await expect(fetchAuctionHistory("m")).rejects.toThrow();
  });
});

describe("toBatchSummary", () => {
  const defaults = { gridStepBps: 10, gridSize: 17 };

  it("keeps a settled auction's curves and clearing", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => Response.json([settled])),
    );
    const [batch] = await fetchAuctionHistory(settled.market);
    const summary = toBatchSummary(batch!, defaults);
    expect(summary).toMatchObject({
      auctionId: 420n,
      state: AuctionState.Settled,
      clearingPrice: 83_591n,
      clearingVolume: 4_000n,
      clearingIndex: 8,
    });
    expect(summary.decryptedSupply[0]).toBe(2_000n);
  });

  it("reads empty auctions as zero-volume at the reference and keeps cancellations", async () => {
    const empty = {
      market: settled.market,
      auctionId: "7",
      outcome: "empty",
      referencePrice: "83000",
      referenceFromOracle: true,
      slot: "1",
      signature: "s",
    };
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json([
          empty,
          { ...empty, auctionId: "8", outcome: "cancelled" },
        ]),
      ),
    );
    const [emptyRecord, cancelled] = await fetchAuctionHistory(settled.market);
    const summary = toBatchSummary(emptyRecord!, defaults);
    expect(summary).toMatchObject({
      state: AuctionState.Settled,
      clearingPrice: 83_000n,
      clearingVolume: 0n,
      clearingIndex: 8,
      gridStepBps: 10,
      gridSize: 17,
    });
    expect(summary.decryptedDemand).toHaveLength(17);
    expect(toBatchSummary(cancelled!, defaults).state).toBe(
      AuctionState.Cancelled,
    );
  });
});
