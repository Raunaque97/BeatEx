import { BASE_UNIT_SIZE } from "@beatex/sdk";

import type { MarketConfig } from "@/lib/config/markets";

/**
 * One market's slice of the trader's BeatEx funds: its mints, what is free to
 * trade or move, and what an open order has locked, all in token atoms.
 */
export interface MarketFunds {
  readonly market: MarketConfig;
  readonly baseMint: string;
  readonly quoteMint: string;
  readonly freeBase: bigint;
  readonly freeQuote: bigint;
  readonly lockedBase: bigint;
  readonly lockedQuote: bigint;
  /** Quote atoms per 100-satoshi base unit, or null with no price yet. */
  readonly price: bigint | null;
}

export interface TokenFunds {
  readonly mint: string;
  readonly symbol: string;
  readonly decimals: number;
  /** Quoted against (a market's quote token), so shown to cents like tUSD. */
  readonly quote: boolean;
  readonly free: bigint;
  readonly locked: bigint;
  /** Free amounts per market holding this token, largest first. */
  readonly byMarket: readonly {
    readonly market: MarketConfig;
    readonly free: bigint;
  }[];
}

/** Every token across every market, keyed by mint: one balance per token. */
export function fundsByToken(
  markets: readonly MarketFunds[],
): Map<string, TokenFunds> {
  const tokens = new Map<
    string,
    {
      mint: string;
      symbol: string;
      decimals: number;
      quote: boolean;
      free: bigint;
      locked: bigint;
      byMarket: { market: MarketConfig; free: bigint }[];
    }
  >();
  const add = (
    funds: MarketFunds,
    mint: string,
    token: MarketConfig["base"],
    free: bigint,
    locked: bigint,
    quote: boolean,
  ) => {
    const entry = tokens.get(mint) ?? {
      mint,
      symbol: token.symbol,
      decimals: token.decimals,
      quote,
      free: 0n,
      locked: 0n,
      byMarket: [],
    };
    entry.quote ||= quote;
    entry.free += free;
    entry.locked += locked;
    entry.byMarket.push({ market: funds.market, free });
    tokens.set(mint, entry);
  };
  for (const funds of markets) {
    add(
      funds,
      funds.baseMint,
      funds.market.base,
      funds.freeBase,
      funds.lockedBase,
      false,
    );
    add(
      funds,
      funds.quoteMint,
      funds.market.quote,
      funds.freeQuote,
      funds.lockedQuote,
      true,
    );
  }
  for (const entry of tokens.values()) {
    entry.byMarket.sort((left, right) =>
      left.free === right.free ? 0 : left.free > right.free ? -1 : 1,
    );
  }
  return tokens;
}

/**
 * The moves needed before an order in `market` can lock `required` atoms of
 * `mint`: none when the market already holds enough, otherwise every other
 * market's whole free balance of that token. Moving everything (not just the
 * shortfall) keeps the public move from revealing the sealed order's size.
 * Returns null when all markets together still fall short.
 */
export function planMoves(
  tokens: ReadonlyMap<string, TokenFunds>,
  market: string,
  mint: string,
  required: bigint,
): { readonly from: MarketConfig; readonly amount: bigint }[] | null {
  const token = tokens.get(mint);
  if (token === undefined) return required === 0n ? [] : null;
  const here =
    token.byMarket.find((entry) => entry.market.address === market)?.free ?? 0n;
  if (here >= required) return [];
  if (token.free < required) return null;
  return token.byMarket
    .filter((entry) => entry.market.address !== market && entry.free > 0n)
    .map((entry) => ({ from: entry.market, amount: entry.free }));
}

/** Total value in quote atoms, pricing each market's base at that market's price. */
export function portfolioValue(markets: readonly MarketFunds[]): bigint | null {
  let total = 0n;
  for (const funds of markets) {
    const base = funds.freeBase + funds.lockedBase;
    if (base > 0n) {
      if (funds.price === null) return null;
      total += (base * funds.price) / BASE_UNIT_SIZE;
    }
    total += funds.freeQuote + funds.lockedQuote;
  }
  return total;
}
