import { describe, expect, it } from "vitest";

import type { MarketConfig } from "@/lib/config/markets";

import {
  fundsByToken,
  planMoves,
  portfolioValue,
  type MarketFunds,
} from "./funds";

const TBTC = "tbtc-mint";
const BEAT = "beat-mint";
const TUSD = "tusd-mint";

const tbtcMarket: MarketConfig = {
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  address: "tbtc-market",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
};
const beatMarket: MarketConfig = {
  slug: "beat-tusd",
  label: "BEAT / tUSD",
  address: "beat-market",
  base: { symbol: "BEAT", decimals: 2 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: false,
};

const funds = (
  market: MarketConfig,
  baseMint: string,
  values: Partial<MarketFunds> = {},
): MarketFunds => ({
  market,
  baseMint,
  quoteMint: TUSD,
  freeBase: 0n,
  freeQuote: 0n,
  lockedBase: 0n,
  lockedQuote: 0n,
  price: null,
  ...values,
});

const markets = [
  funds(tbtcMarket, TBTC, {
    freeBase: 5_000_000n,
    freeQuote: 6_735_680_000n,
    lockedQuote: 100_000_000n,
    price: 83_000n,
  }),
  funds(beatMarket, BEAT, { freeQuote: 0n, price: 120n }),
];

describe("funds across markets", () => {
  it("shows one tUSD balance however many markets hold it", () => {
    const tokens = fundsByToken(markets);
    expect(tokens.get(TUSD)).toMatchObject({
      symbol: "tUSD",
      free: 6_735_680_000n,
      locked: 100_000_000n,
    });
    expect(tokens.get(BEAT)?.free).toBe(0n);
  });

  it("moves another market's whole free balance when this one is short", () => {
    const tokens = fundsByToken(markets);
    expect(planMoves(tokens, "beat-market", TUSD, 2_000_000_000n)).toEqual([
      { from: tbtcMarket, amount: 6_735_680_000n },
    ]);
    // Enough already here: nothing to move.
    expect(planMoves(tokens, "tbtc-market", TUSD, 2_000_000_000n)).toEqual([]);
    // Not enough anywhere.
    expect(planMoves(tokens, "beat-market", TUSD, 9_000_000_000n)).toBeNull();
  });

  it("values each market's base at its own price", () => {
    // 0.05 tBTC at 83,000 + 6,835.68 tUSD (free and locked).
    expect(portfolioValue(markets)).toBe(4_150_000_000n + 6_835_680_000n);
    expect(
      portfolioValue([funds(beatMarket, BEAT, { freeBase: 100n })]),
    ).toBeNull();
  });
});
