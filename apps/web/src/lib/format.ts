import {
  baseUnitsToTokens,
  formatDecimalUnits,
  parseDecimalUnits,
  priceUnitsToTokens,
  quoteUnitsToTokens,
  TBTC_DECIMALS,
} from "@beatex/sdk";

/** Group the integer part of a decimal string: "85500.5" -> "85,500.5". */
export function groupDecimal(value: string): string {
  const [whole = "0", fraction] = value.split(".");
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return fraction === undefined ? grouped : `${grouped}.${fraction}`;
}

/** Integer quote units per base unit, shown as tUSD per tBTC. */
export function formatPrice(price: bigint): string {
  const [whole = "0"] = priceUnitsToTokens(price).split(".");
  return groupDecimal(whole);
}

/** Base units (100 satoshis) as a tBTC amount. Order quantities use base units. */
export function formatBase(units: bigint): string {
  return groupDecimal(baseUnitsToTokens(units));
}

/**
 * Token atoms (satoshis) as a tBTC amount. Deposits, withdrawals, private
 * balances, and order reserves are all in atoms, not base units.
 */
export function formatBaseAtoms(atoms: bigint): string {
  return groupDecimal(baseAtomsToTokens(atoms));
}

export function baseAtomsToTokens(atoms: bigint): string {
  return formatDecimalUnits(atoms, TBTC_DECIMALS);
}

export function baseTokensToAtoms(value: string): bigint {
  return parseDecimalUnits(value, TBTC_DECIMALS);
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
  symbols: { readonly base: string; readonly quote: string },
): string {
  const parts = [
    baseAtoms > 0n ? `${formatBaseAtoms(baseAtoms)} ${symbols.base}` : null,
    quoteAtoms > 0n ? `${formatQuoteShort(quoteAtoms)} ${symbols.quote}` : null,
  ].filter((part) => part !== null);
  return parts.join(" and ");
}
