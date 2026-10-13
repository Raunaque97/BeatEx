/**
 * Illustrative numbers for the home page's clearing story. The rules mirror
 * the program (TECHNICAL_DESIGN.md §7.2, §11, §12); the curves are made up.
 */

export const DEMO_LEVELS = 17;
export const DEMO_CENTER = 8;
export const DEMO_STEP_BPS = 10;
export const DEMO_REFERENCE_PRICE = 85_500;

/** Grid price at `index`: linear bps offsets from the reference, rounded half up. */
export function gridPrice(reference: number, index: number): number {
  const multiplier = 10_000 + (index - DEMO_CENTER) * DEMO_STEP_BPS;
  return Math.floor((reference * multiplier + 5_000) / 10_000);
}

export const DEMO_GRID: readonly number[] = Array.from(
  { length: DEMO_LEVELS },
  (_, index) => gridPrice(DEMO_REFERENCE_PRICE, index),
);

/** Aggregate base quantity bid at each exact level. */
export const DEMO_DEMAND: readonly number[] = [
  66, 64, 61, 58, 55, 52, 50, 47, 42, 36, 25, 21, 15, 10, 6, 4, 2,
];

/** Aggregate base quantity offered at each exact level. */
export const DEMO_SUPPLY: readonly number[] = [
  1, 2, 3, 4, 6, 9, 12, 16, 22, 27, 34, 40, 45, 50, 54, 57, 60,
];

export interface Clearing {
  readonly index: number;
  readonly price: number;
  readonly volume: number;
}

/**
 * The program's rule: the level with the most executable volume
 * `min(demand, supply)`; ties go to the price nearest `tieBreakPrice`, then
 * the lower price. With no volume anywhere, the price nearest the tie-break.
 */
export function clearAuction(
  grid: readonly number[],
  demand: readonly number[],
  supply: readonly number[],
  tieBreakPrice: number,
): Clearing {
  let best: Clearing | null = null;
  let bestDistance = Infinity;
  for (const [index, price] of grid.entries()) {
    const volume = Math.min(demand[index] ?? 0, supply[index] ?? 0);
    const distance = Math.abs(price - tieBreakPrice);
    if (
      best === null ||
      volume > best.volume ||
      (volume === best.volume &&
        (distance < bestDistance ||
          (distance === bestDistance && price < best.price)))
    ) {
      best = { index, price, volume };
      bestDistance = distance;
    }
  }
  if (best === null) throw new Error("An auction grid has at least one level.");
  return best;
}

export const DEMO_CLEARING = clearAuction(
  DEMO_GRID,
  DEMO_DEMAND,
  DEMO_SUPPLY,
  DEMO_REFERENCE_PRICE,
);
