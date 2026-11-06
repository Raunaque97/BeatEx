import { BASE_UNIT_SIZE } from "@beatex/sdk";

import type { MarketConfig } from "@/lib/config/markets";
import { formatBaseAtoms, formatQuoteShort } from "@/lib/format";

/** What an order's reserves were and what settlement paid back, in atoms. */
export interface SettlementAmounts {
  readonly baseReserve: bigint;
  readonly quoteReserve: bigint;
  readonly basePayout: bigint;
  readonly quotePayout: bigint;
}

export interface SettlementOutcome {
  readonly auctionId: bigint;
  readonly kind: "bought" | "sold" | "unfilled" | "refunded";
  /** Base units traded; zero unless bought or sold. */
  readonly baseFilled: bigint;
  /** One sentence for the trader. */
  readonly summary: string;
}

/**
 * Describe a settled or refunded order. A buy locks tUSD and is paid tBTC; a
 * sell locks tBTC and is paid tUSD. Whatever was not traded comes back.
 */
export function describeSettlement(
  auctionId: bigint,
  amounts: SettlementAmounts,
  refunded: boolean,
  market: MarketConfig,
): SettlementOutcome {
  const batch = `Auction #${auctionId}`;
  const returned = [
    amounts.basePayout > 0n
      ? `${formatBaseAtoms(amounts.basePayout)} ${market.base.symbol}`
      : null,
    amounts.quotePayout > 0n
      ? `${formatQuoteShort(amounts.quotePayout)} ${market.quote.symbol}`
      : null,
  ]
    .filter((part) => part !== null)
    .join(" and ");
  if (refunded) {
    return {
      auctionId,
      kind: "refunded",
      baseFilled: 0n,
      summary: `${batch} was cancelled. ${returned || "Your funds"} returned to your balance.`,
    };
  }
  const baseIn = amounts.basePayout - amounts.baseReserve;
  const quoteIn = amounts.quotePayout - amounts.quoteReserve;
  if (baseIn > 0n && quoteIn < 0n) {
    return {
      auctionId,
      kind: "bought",
      baseFilled: baseIn / BASE_UNIT_SIZE,
      summary: `${batch} filled: bought ${formatBaseAtoms(baseIn)} ${market.base.symbol} for ${formatQuoteShort(-quoteIn)} ${market.quote.symbol}.`,
    };
  }
  if (baseIn < 0n && quoteIn > 0n) {
    return {
      auctionId,
      kind: "sold",
      baseFilled: -baseIn / BASE_UNIT_SIZE,
      summary: `${batch} filled: sold ${formatBaseAtoms(-baseIn)} ${market.base.symbol} for ${formatQuoteShort(quoteIn)} ${market.quote.symbol}.`,
    };
  }
  return {
    auctionId,
    kind: "unfilled",
    baseFilled: 0n,
    summary: `${batch}: there were no other orders for yours to match. ${returned || "Your funds"} returned to your balance.`,
  };
}
