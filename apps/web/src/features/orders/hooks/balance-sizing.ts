import {
  BASE_UNIT_SIZE,
  baseTokensToUnits,
  baseUnitsToTokens,
  quoteTokensToUnits,
  quoteUnitsToTokens,
  type AuctionGridLevel,
} from "@beatex/sdk";

import { percentOf, shareOf } from "@/components/percent-slider";
import type { MarketConfig } from "@/lib/config/markets";
import { baseAtomsToTokens, baseTokensToAtoms } from "@/lib/format";
import type { Holdings } from "@/lib/portfolio";

import type { OrderIntentState } from "./use-order-intent";

function parseOr0(parse: (value: string) => bigint, text: string): bigint {
  try {
    return parse(text || "0");
  } catch {
    return 0n;
  }
}

/** Whole 100-satoshi base units, as a tBTC amount. */
function wholeUnits(atoms: bigint): string {
  return baseAtomsToTokens((atoms / BASE_UNIT_SIZE) * BASE_UNIT_SIZE);
}

/**
 * Size a market or limit order as a share of the BeatEx balance being spent:
 * tUSD for a buy, tBTC for a sell.
 */
export function spendSizing(
  order: OrderIntentState,
  market: MarketConfig,
  balance: Holdings | null,
) {
  const { intent, built } = order;
  const spendsQuote = intent.side === "buy";
  const spendUnit = spendsQuote ? "quote" : "base";
  const symbol = spendsQuote ? market.quote.symbol : market.base.symbol;
  const available =
    balance === null
      ? null
      : spendsQuote
        ? balance.quoteAtoms
        : balance.baseAtoms;
  const entered =
    intent.unit === spendUnit
      ? parseOr0(
          spendsQuote ? quoteTokensToUnits : baseTokensToAtoms,
          intent.amount,
        )
      : // Entered in the other asset: size by what the order would lock.
        built?.ok
        ? spendsQuote
          ? built.order.quoteReserve
          : built.order.baseReserve * BASE_UNIT_SIZE
        : 0n;
  const percent = available === null ? 0 : percentOf(entered, available);
  const size = (next: number) => {
    if (available === null) return;
    const atoms = shareOf(available, next);
    if (intent.unit !== spendUnit) order.setUnit(spendUnit);
    order.setAmount(
      atoms === 0n
        ? ""
        : spendsQuote
          ? quoteUnitsToTokens(atoms)
          : // Orders trade whole 100-satoshi base units.
            wholeUnits(atoms),
    );
  };
  return { symbol, available, percent, size };
}

/**
 * Size a custom curve's Max sell from the tBTC balance, and its Max buy from
 * the tUSD balance at the highest grid price, so even a full buy at the top
 * level stays within what the trader holds.
 */
export function customSizing(
  order: OrderIntentState,
  balance: Holdings | null,
  grid: readonly AuctionGridLevel[] | null,
) {
  const { intent } = order;
  const top =
    grid === null || grid.length === 0
      ? null
      : grid.reduce(
          (max, level) => (level.price > max ? level.price : max),
          grid[0]!.price,
        );
  const sellAtoms = balance?.baseAtoms ?? null;
  // Base units the tUSD balance can buy at the top level.
  const buyUnits =
    balance === null || top === null || top === 0n
      ? null
      : balance.quoteAtoms / top;

  const sellPercent =
    sellAtoms === null
      ? 0
      : percentOf(parseOr0(baseTokensToAtoms, intent.maxSell), sellAtoms);
  const buyPercent =
    buyUnits === null
      ? 0
      : percentOf(parseOr0(baseTokensToUnits, intent.maxBuy), buyUnits);

  return {
    sell: {
      available: sellAtoms,
      percent: sellPercent,
      size: (next: number) => {
        if (sellAtoms === null) return;
        const atoms = shareOf(sellAtoms, next);
        order.setMaxSell(atoms === 0n ? "" : wholeUnits(atoms));
      },
    },
    buy: {
      /** tBTC the tUSD balance covers at the top price, in base units. */
      available: buyUnits,
      percent: buyPercent,
      size: (next: number) => {
        if (buyUnits === null) return;
        const units = shareOf(buyUnits, next);
        order.setMaxBuy(units === 0n ? "" : baseUnitsToTokens(units));
      },
    },
  };
}
