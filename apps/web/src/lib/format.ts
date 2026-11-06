import {
  BASE_UNIT_SIZE,
  formatDecimalUnits,
  parseDecimalUnits,
  quoteUnitsToTokens,
} from "@beatex/sdk";

import type { MarketConfig } from "@/lib/config/markets";

/** The token decimals that give a market's integer amounts their scale. */
export type MarketUnits = Pick<MarketConfig, "base" | "quote">;

/** Base-token decimal digits that one base unit (100 atoms) spans. */
const BASE_UNIT_DIGITS = BASE_UNIT_SIZE.toString().length - 1;

/** Group the integer part of a decimal string: "85500.5" -> "85,500.5". */
export function groupDecimal(value: string): string {
  const [whole = "0", fraction] = value.split(".");
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return fraction === undefined ? grouped : `${grouped}.${fraction}`;
}

/**
 * Decimal places a base amount can have, since orders trade whole base units:
 * 6 for tBTC (100 satoshis), 0 for BEAT (100 atoms is 1.00 BEAT).
 */
export function baseQuantityDecimals(market: MarketUnits): number {
  return market.base.decimals - BASE_UNIT_DIGITS;
}

/**
 * Integer quote units per base unit, shown exactly as quote tokens per base
 * token. How far the point moves depends on the market: 85,500 is 85,500 tUSD
 * per tBTC, but 1,002,001 is 1.002001 tUSD per BEAT.
 */
export function formatPrice(price: bigint, market: MarketUnits): string {
  const decimals = market.quote.decimals - baseQuantityDecimals(market);
  const [whole = "0", fraction = ""] = formatDecimalUnits(
    price,
    decimals,
  ).split(".");
  return groupDecimal(
    decimals === 0 ? whole : `${whole}.${fraction.padEnd(decimals, "0")}`,
  );
}

/** Base units as a base-token amount. Order quantities use base units. */
export function formatBase(units: bigint, market: MarketUnits): string {
  return groupDecimal(baseUnitsToTokens(units, market));
}

/**
 * Token atoms as a base-token amount. Deposits, withdrawals, private balances,
 * and order reserves are all in atoms, not base units.
 */
export function formatBaseAtoms(atoms: bigint, market: MarketUnits): string {
  return groupDecimal(baseAtomsToTokens(atoms, market));
}

export function baseAtomsToTokens(atoms: bigint, market: MarketUnits): string {
  return formatDecimalUnits(atoms, market.base.decimals);
}

export function baseTokensToAtoms(value: string, market: MarketUnits): bigint {
  return parseDecimalUnits(value, market.base.decimals);
}

export function baseUnitsToTokens(units: bigint, market: MarketUnits): string {
  return baseAtomsToTokens(units * BASE_UNIT_SIZE, market);
}

/** Convert a base-token amount to base units, rejecting a fraction of a unit. */
export function baseTokensToUnits(value: string, market: MarketUnits): bigint {
  const atoms = baseTokensToAtoms(value, market);
  if (atoms % BASE_UNIT_SIZE !== 0n) {
    throw new RangeError("amount must be an exact multiple of the base unit");
  }
  return atoms / BASE_UNIT_SIZE;
}

/** Quote units (micro-tUSD) as a tUSD amount with two decimals. */
export function formatQuote(units: bigint): string {
  const [whole = "0", fraction = ""] = quoteUnitsToTokens(units).split(".");
  return groupDecimal(`${whole}.${fraction.padEnd(2, "0").slice(0, 2)}`);
}

export function formatBps(offset: number, stepBps: number): string {
  const bps = offset * stepBps;
  return `${bps > 0 ? "+" : ""}${bps} bps`;
}

/** Quote units as a tUSD amount, dropping a zero fraction: 5000.00 -> "5,000". */
export function formatQuoteShort(units: bigint): string {
  return formatQuote(units).replace(/\.00$/, "");
}

/** "0.05 tBTC and 5,000 tUSD", skipping an asset with nothing to move. */
export function describeAmounts(
  baseAtoms: bigint,
  quoteAtoms: bigint,
  market: MarketUnits,
): string {
  const parts = [
    baseAtoms > 0n
      ? `${formatBaseAtoms(baseAtoms, market)} ${market.base.symbol}`
      : null,
    quoteAtoms > 0n
      ? `${formatQuoteShort(quoteAtoms)} ${market.quote.symbol}`
      : null,
  ].filter((part) => part !== null);
  return parts.join(" and ");
}
