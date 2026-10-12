import { describe, expect, it } from "vitest";

import type { MarketConfig } from "@/lib/config/markets";

import { describeSettlement } from "./settlement-outcome";

const market: MarketConfig = {
  address: "market",
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
};

describe("describeSettlement", () => {
  it("reports a filled buy", () => {
    const outcome = describeSettlement(
      5725n,
      {
        baseReserve: 0n,
        quoteReserve: 500_000_000n,
        basePayout: 590_000n,
        quotePayout: 10_000_000n,
      },
      false,
      market,
    );
    expect(outcome.kind).toBe("bought");
    expect(outcome.summary).toBe(
      "Auction #5725 filled: bought 0.0059 tBTC for 490 tUSD.",
    );
  });

  it("reports a filled sell", () => {
    const outcome = describeSettlement(
      7n,
      {
        baseReserve: 1_000_000n,
        quoteReserve: 0n,
        basePayout: 0n,
        quotePayout: 835_190_000n,
      },
      false,
      market,
    );
    expect(outcome.summary).toBe(
      "Auction #7 filled: sold 0.01 tBTC for 835.19 tUSD.",
    );
  });

  it("reports an unfilled order and what came back", () => {
    const outcome = describeSettlement(
      5725n,
      {
        baseReserve: 0n,
        quoteReserve: 490_000_000n,
        basePayout: 0n,
        quotePayout: 490_000_000n,
      },
      false,
      market,
    );
    expect(outcome.kind).toBe("unfilled");
    expect(outcome.summary).toBe(
      "Auction #5725 didn't fill your order. 490 tUSD returned to your balance.",
    );
  });

  it("reports a refund from a cancelled batch", () => {
    expect(
      describeSettlement(
        9n,
        {
          baseReserve: 0n,
          quoteReserve: 1n,
          basePayout: 0n,
          quotePayout: 1_000_000n,
        },
        true,
        market,
      ).summary,
    ).toBe("Auction #9 was cancelled. 1 tUSD returned to your balance.");
  });
});
