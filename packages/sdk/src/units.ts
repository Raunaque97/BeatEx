import { asU64, U64_MAX } from "./boundaries/primitives.js";
import type { AuctionAccount, MarketAccount } from "./protocol/accounts.js";

export const TBTC_DECIMALS = 8;
export const TUSD_DECIMALS = 6;
export const TBTC_BASE_UNIT_SIZE = 100n;
export const TUSD_QUOTE_UNIT_SIZE = 1n;
export const BASIS_POINTS_DENOMINATOR = 10_000n;

export interface AuctionGridLevel {
  readonly index: number;
  readonly offset: number;
  /** Integer quote units per base unit. */
  readonly price: bigint;
  /** Human-readable tUSD per tBTC. */
  readonly displayPrice: string;
}

function pow10(decimals: number): bigint {
  if (!Number.isSafeInteger(decimals) || decimals < 0) {
    throw new RangeError("decimals must be a non-negative safe integer");
  }
  return 10n ** BigInt(decimals);
}

export function parseDecimalUnits(value: string, decimals: number, label = "amount"): bigint {
  if (!/^(?:0|[1-9][0-9]*)(?:\.[0-9]+)?$/.test(value)) {
    throw new RangeError(`${label} must be a non-negative decimal string`);
  }
  const [wholePart, fraction = ""] = value.split(".");
  const whole = wholePart ?? "0";
  if (fraction.length > decimals) {
    throw new RangeError(`${label} has more than ${decimals} decimal places`);
  }
  const scale = pow10(decimals);
  return BigInt(whole) * scale + BigInt(fraction.padEnd(decimals, "0") || "0");
}

export function formatDecimalUnits(value: bigint, decimals: number): string {
  if (value < 0n) throw new RangeError("amount cannot be negative");
  const scale = pow10(decimals);
  const whole = value / scale;
  const fraction = (value % scale).toString().padStart(decimals, "0").replace(/0+$/, "");
  return fraction.length === 0 ? whole.toString() : `${whole}.${fraction}`;
}

function exactUnits(tokenAtoms: bigint, unitSize: bigint, label: string): bigint {
  if (unitSize <= 0n) throw new RangeError(`${label} unit size must be positive`);
  if (tokenAtoms % unitSize !== 0n) {
    throw new RangeError(`${label} must be an exact multiple of its on-chain unit size`);
  }
  return asU64(tokenAtoms / unitSize, label);
}

/** Convert a human tBTC quantity into counts of 100-satoshi base units. */
export function baseTokensToUnits(value: string): bigint {
  return exactUnits(parseDecimalUnits(value, TBTC_DECIMALS, "base quantity"), TBTC_BASE_UNIT_SIZE, "base quantity");
}

/** Format counts of 100-satoshi base units as a human tBTC quantity. */
export function baseUnitsToTokens(value: bigint): string {
  return formatDecimalUnits(asU64(value, "base units") * TBTC_BASE_UNIT_SIZE, TBTC_DECIMALS);
}

/** Convert a human tUSD quantity into counts of one-micro-tUSD quote units. */
export function quoteTokensToUnits(value: string): bigint {
  return exactUnits(parseDecimalUnits(value, TUSD_DECIMALS, "quote quantity"), TUSD_QUOTE_UNIT_SIZE, "quote quantity");
}

/** Format counts of one-micro-tUSD quote units as a human tUSD quantity. */
export function quoteUnitsToTokens(value: bigint): string {
  return formatDecimalUnits(asU64(value, "quote units") * TUSD_QUOTE_UNIT_SIZE, TUSD_DECIMALS);
}

/** Convert human tUSD/tBTC to integer quote units per base unit. */
export function tokenPriceToUnits(value: string): bigint {
  const quoteAtomsPerToken = parseDecimalUnits(value, TUSD_DECIMALS, "price");
  const numerator = quoteAtomsPerToken * TBTC_BASE_UNIT_SIZE;
  const denominator = TUSD_QUOTE_UNIT_SIZE * pow10(TBTC_DECIMALS);
  if (numerator % denominator !== 0n) {
    throw new RangeError("price cannot be represented exactly in integer quote units per base unit");
  }
  return asU64(numerator / denominator, "price");
}

/** Format integer quote units per base unit as human tUSD/tBTC. */
export function priceUnitsToTokens(value: bigint): string {
  const price = asU64(value, "price");
  const numerator = price * TUSD_QUOTE_UNIT_SIZE * pow10(TBTC_DECIMALS);
  if (numerator % TBTC_BASE_UNIT_SIZE !== 0n) {
    throw new RangeError("price units do not map to the configured token precision");
  }
  return formatDecimalUnits(numerator / TBTC_BASE_UNIT_SIZE, TUSD_DECIMALS);
}

export function priceAtGridOffset(
  referencePrice: bigint,
  gridStepBps: number,
  offset: number,
): bigint {
  asU64(referencePrice, "referencePrice");
  if (!Number.isSafeInteger(gridStepBps) || gridStepBps < 1 || gridStepBps > 65_535) {
    throw new RangeError("gridStepBps must be a positive u16");
  }
  if (!Number.isSafeInteger(offset)) throw new RangeError("grid offset must be a safe integer");
  const multiplier = BASIS_POINTS_DENOMINATOR + BigInt(offset) * BigInt(gridStepBps);
  if (referencePrice === 0n || multiplier <= 0n) {
    throw new RangeError("grid price must be positive");
  }
  const numerator = referencePrice * multiplier;
  const price = (numerator + BASIS_POINTS_DENOMINATOR / 2n) / BASIS_POINTS_DENOMINATOR;
  if (price === 0n || price > U64_MAX) throw new RangeError("grid price exceeds u64");
  return price;
}

/** Build the immutable auction grid and reject adjacent offsets rounded to one price. */
export function buildAuctionGrid(
  auction: Pick<AuctionAccount, "referencePrice" | "gridStepBps" | "gridSize">,
): readonly AuctionGridLevel[] {
  if (!Number.isSafeInteger(auction.gridSize) || auction.gridSize < 1 || auction.gridSize % 2 !== 1) {
    throw new RangeError("gridSize must be a positive odd integer");
  }
  const radius = Math.floor(auction.gridSize / 2);
  const levels = Array.from({ length: auction.gridSize }, (_, index) => {
    const offset = index - radius;
    const price = priceAtGridOffset(auction.referencePrice, auction.gridStepBps, offset);
    return { index, offset, price, displayPrice: priceUnitsToTokens(price) };
  });
  for (let index = 1; index < levels.length; index += 1) {
    if (levels[index - 1]!.price >= levels[index]!.price) {
      throw new RangeError("grid basis-point step produces duplicate or unordered integer prices");
    }
  }
  return levels;
}

export function assertStandardTokenUnits(
  market: Pick<MarketAccount, "baseUnitSize" | "quoteUnitSize">,
): void {
  if (market.baseUnitSize !== TBTC_BASE_UNIT_SIZE || market.quoteUnitSize !== TUSD_QUOTE_UNIT_SIZE) {
    throw new RangeError(
      `unsupported token unit sizes: expected base=${TBTC_BASE_UNIT_SIZE}, quote=${TUSD_QUOTE_UNIT_SIZE}`,
    );
  }
}
