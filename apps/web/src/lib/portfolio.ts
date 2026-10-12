import { BASE_UNIT_SIZE } from "@beatex/sdk";

/** Token atoms held of a market's base (satoshis) and quote (micro-tUSD) tokens. */
export interface Holdings {
  readonly baseAtoms: bigint;
  readonly quoteAtoms: bigint;
}

/**
 * Value in quote atoms. `price` is quote atoms per 100-satoshi base unit, which
 * reads as tUSD per tBTC. Rounds down.
 */
export function valueInQuote(holdings: Holdings, price: bigint): bigint {
  return holdings.quoteAtoms + (holdings.baseAtoms * price) / BASE_UNIT_SIZE;
}

/** Sum holdings, e.g. the free balance plus reserves locked in an open order. */
export function addHoldings(...parts: readonly Holdings[]): Holdings {
  return parts.reduce(
    (total, part) => ({
      baseAtoms: total.baseAtoms + part.baseAtoms,
      quoteAtoms: total.quoteAtoms + part.quoteAtoms,
    }),
    { baseAtoms: 0n, quoteAtoms: 0n },
  );
}
