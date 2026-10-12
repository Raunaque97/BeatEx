import { GENERATED_MARKET_ID } from "@beatex/sdk";

import { publicEnv } from "./public-env";

export interface TokenConfig {
  readonly symbol: string;
  readonly decimals: number;
}

export interface MarketConfig {
  readonly slug: string;
  readonly label: string;
  readonly address: string;
  readonly base: TokenConfig;
  readonly quote: TokenConfig;
  /** Whether the test-token faucet dispenses this market's tokens. BEAT has
   * no faucet: users buy it on the DEX with faucet tUSD. */
  readonly faucet: boolean;
}

/**
 * Markets offered by the selector: the deployment manifest's oracle-priced
 * tBTC/tUSD market, plus the oracle-free BEAT/tUSD market when configured.
 */
export const MARKETS: readonly MarketConfig[] = [
  ...(GENERATED_MARKET_ID === null
    ? []
    : [
        {
          slug: "tbtc-tusd",
          label: "tBTC / tUSD",
          address: GENERATED_MARKET_ID,
          base: { symbol: "tBTC", decimals: 8 },
          quote: { symbol: "tUSD", decimals: 6 },
          faucet: true,
        },
      ]),
  ...(publicEnv.beatMarketId === undefined
    ? []
    : [
        {
          slug: "beat-tusd",
          label: "BEAT / tUSD",
          address: publicEnv.beatMarketId,
          base: { symbol: "BEAT", decimals: 2 },
          quote: { symbol: "tUSD", decimals: 6 },
          faucet: false,
        },
      ]),
];

export const DEFAULT_MARKET_SLUG = MARKETS[0]?.slug ?? "tbtc-tusd";

export function findMarket(slug: string): MarketConfig | undefined {
  return MARKETS.find((market) => market.slug === slug);
}
