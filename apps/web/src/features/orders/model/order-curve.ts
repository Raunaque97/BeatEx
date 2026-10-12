import {
  PRICE_LEVELS,
  baseTokensToUnits,
  baseUnitsToTokens,
  quoteTokensToUnits,
  type AuctionGridLevel,
  type OrderCurve,
} from "@beatex/sdk";

export type OrderType = "market" | "limit" | "custom";
export type OrderSide = "buy" | "sell";
/** Which token the typed amount is denominated in. */
export type AmountUnit = "quote" | "base";

export interface OrderIntent {
  readonly type: OrderType;
  readonly side: OrderSide;
  readonly amount: string;
  readonly unit: AmountUnit;
  /** Grid index of the limit price. */
  readonly limitIndex: number;
  /**
   * Custom curve as a signed percentage per level: positive buys that share of
   * `maxBuy`, negative sells that share of `maxSell`.
   */
  readonly customLevels: readonly number[];
  /** tBTC amount a custom level buys at 100%. */
  readonly maxBuy: string;
  /** tBTC amount a custom level sells at 100%. */
  readonly maxSell: string;
}

export interface BuiltOrder {
  readonly curve: OrderCurve;
  /** tBTC (base units) the order locks: the largest sell entry. */
  readonly baseReserve: bigint;
  /** tUSD (quote units) the order locks: the largest `price * buy` entry. */
  readonly quoteReserve: bigint;
}

export type BuildResult =
  | { readonly ok: true; readonly order: BuiltOrder }
  | { readonly ok: false; readonly error: string };

export const CENTER_INDEX = (PRICE_LEVELS - 1) / 2;

export function emptyCustomLevels(): readonly number[] {
  return Array.from({ length: PRICE_LEVELS }, () => 0);
}

/** Whether `index` is a level the intent trades at (ignores the amount). */
export function levelIsActive(intent: OrderIntent, index: number): boolean {
  if (intent.type === "market") return true;
  if (intent.type === "limit") {
    return intent.side === "buy"
      ? index <= intent.limitIndex
      : index >= intent.limitIndex;
  }
  return (intent.customLevels[index] ?? 0) !== 0;
}

/**
 * Convert a market, limit, or custom intent into the 17-level curve the program
 * accepts. Quote-denominated amounts become a different base quantity per level
 * (`floor(quote / price)`), so a budget is never exceeded at any clearing price.
 */
export function buildOrder(
  intent: OrderIntent,
  grid: readonly AuctionGridLevel[],
  maxOrderQuantity: bigint,
): BuildResult {
  if (grid.length !== PRICE_LEVELS) {
    return { ok: false, error: "The auction price grid is not available yet." };
  }
  const buy: bigint[] = [];
  const sell: bigint[] = [];

  if (intent.type === "custom") {
    const usesBuy = intent.customLevels.some((percent) => percent > 0);
    const usesSell = intent.customLevels.some((percent) => percent < 0);
    const maxBuy = usesBuy ? parseMax(intent.maxBuy, "buy") : 0n;
    const maxSell = usesSell ? parseMax(intent.maxSell, "sell") : 0n;
    if (typeof maxBuy === "string") return { ok: false, error: maxBuy };
    if (typeof maxSell === "string") return { ok: false, error: maxSell };
    for (let index = 0; index < PRICE_LEVELS; index += 1) {
      const percent = intent.customLevels[index] ?? 0;
      buy.push(percent > 0 ? (maxBuy * BigInt(percent)) / 100n : 0n);
      sell.push(percent < 0 ? (maxSell * BigInt(-percent)) / 100n : 0n);
    }
  } else {
    let amount: bigint;
    try {
      amount =
        intent.unit === "quote"
          ? quoteTokensToUnits(intent.amount.trim())
          : baseTokensToUnits(intent.amount.trim());
    } catch (error) {
      return { ok: false, error: amountError(intent, error) };
    }
    if (amount === 0n) return { ok: false, error: "Enter an amount." };
    for (let index = 0; index < PRICE_LEVELS; index += 1) {
      const level = grid[index]!;
      const quantity = levelIsActive(intent, index)
        ? intent.unit === "quote"
          ? amount / level.price
          : amount
        : 0n;
      buy.push(intent.side === "buy" ? quantity : 0n);
      sell.push(intent.side === "sell" ? quantity : 0n);
    }
  }

  const largest = [...buy, ...sell].reduce(
    (max, value) => (value > max ? value : max),
    0n,
  );
  if (largest === 0n) {
    return {
      ok: false,
      error:
        intent.type === "custom"
          ? "Drag at least one level to buy or sell."
          : "Amount is below the minimum size at these prices.",
    };
  }
  if (largest > maxOrderQuantity) {
    return {
      ok: false,
      error: `This market allows at most ${baseUnitsToTokens(maxOrderQuantity)} tBTC per price level.`,
    };
  }
  return {
    ok: true,
    order: { curve: { buy, sell }, ...reserves({ buy, sell }, grid) },
  };
}

export function reserves(
  curve: OrderCurve,
  grid: readonly AuctionGridLevel[],
): { readonly baseReserve: bigint; readonly quoteReserve: bigint } {
  let baseReserve = 0n;
  let quoteReserve = 0n;
  curve.sell.forEach((quantity) => {
    if (quantity > baseReserve) baseReserve = quantity;
  });
  curve.buy.forEach((quantity, index) => {
    const cost = quantity * grid[index]!.price;
    if (cost > quoteReserve) quoteReserve = cost;
  });
  return { baseReserve, quoteReserve };
}

export interface FillEstimate {
  /** Base units bought or sold if the batch clears at `index`. */
  readonly base: bigint;
  /** Quote units paid or received if the batch clears at `index`. */
  readonly quote: bigint;
}

/** What the order would trade at one clearing index, before pro-rata allocation. */
export function fillAt(
  curve: OrderCurve,
  grid: readonly AuctionGridLevel[],
  index: number,
): FillEstimate {
  const base = (curve.buy[index] ?? 0n) + (curve.sell[index] ?? 0n);
  return { base, quote: base * grid[index]!.price };
}

export interface OutcomeEstimate {
  /** Clearing index the estimate assumes: the reference level if the order trades there. */
  readonly index: number;
  /** What the trader receives: base units for a buy, quote units for a sell. */
  readonly receive: bigint;
  readonly receiveMin: bigint;
  readonly receiveMax: bigint;
  /** Lowest and highest price (tUSD per tBTC) the order can fill at. */
  readonly priceMin: bigint;
  readonly priceMax: bigint;
  /** What the trader gives up at `index`: quote units for a buy, base units for a sell. */
  readonly pay: bigint;
}

/**
 * Expected outcome for a one-sided order if the batch clears at the reference price
 * (or the nearest level the order trades at), with the range over every level it
 * could clear at. Assumes a full fill; pro-rata allocation can reduce it.
 */
export function estimateOutcome(
  order: BuiltOrder,
  grid: readonly AuctionGridLevel[],
  side: OrderSide,
): OutcomeEstimate | null {
  const levels = grid.filter(
    (level) => fillAt(order.curve, grid, level.index).base > 0n,
  );
  if (levels.length === 0) return null;
  const nearest = levels.reduce((best, level) =>
    Math.abs(level.index - CENTER_INDEX) < Math.abs(best.index - CENTER_INDEX)
      ? level
      : best,
  );
  const receiveAt = (index: number) => {
    const fill = fillAt(order.curve, grid, index);
    return side === "buy" ? fill.base : fill.quote;
  };
  const received = levels.map((level) => receiveAt(level.index));
  const fill = fillAt(order.curve, grid, nearest.index);
  return {
    index: nearest.index,
    receive: receiveAt(nearest.index),
    receiveMin: received.reduce((min, value) => (value < min ? value : min)),
    receiveMax: received.reduce((max, value) => (value > max ? value : max)),
    priceMin: levels.reduce(
      (min, level) => (level.price < min ? level.price : min),
      levels[0]!.price,
    ),
    priceMax: levels.reduce(
      (max, level) => (level.price > max ? level.price : max),
      levels[0]!.price,
    ),
    pay: side === "buy" ? fill.quote : fill.base,
  };
}

function parseMax(value: string, side: OrderSide): bigint | string {
  if (value.trim() === "") return `Set a max ${side} amount.`;
  try {
    const units = baseTokensToUnits(value.trim());
    return units === 0n ? `Set a max ${side} amount.` : units;
  } catch {
    return "tBTC amounts go down to 0.000001.";
  }
}

function amountError(intent: OrderIntent, error: unknown): string {
  const message = error instanceof Error ? error.message : "";
  if (message.includes("decimal places") || message.includes("multiple")) {
    return intent.unit === "base"
      ? "tBTC amounts go down to 0.000001."
      : "tUSD amounts go down to 0.000001.";
  }
  return "Enter a valid amount.";
}
