import { PRICE_LEVELS } from "@beatex/sdk";

import { CENTER_INDEX } from "./order-curve";

/**
 * Starting shapes for a custom curve. Each returns one signed percentage per
 * level, lowest price first: positive buys that share of Max buy, negative
 * sells that share of Max sell. The reference price (center level) is never
 * traded, and no level both buys and sells.
 */
export interface CurvePreset {
  readonly id: string;
  readonly label: string;
  readonly description: string;
  readonly levels: () => number[];
}

/** 0 at the reference, 100 at the outermost level. */
function ramp(index: number): number {
  return Math.round((Math.abs(index - CENTER_INDEX) * 100) / CENTER_INDEX);
}

function shape(
  level: (offset: number, index: number) => number,
): () => number[] {
  return () =>
    Array.from({ length: PRICE_LEVELS }, (_, index) => {
      const value = level(index - CENTER_INDEX, index);
      // Avoid -0, which reads as a sell of nothing.
      return value === 0 ? 0 : value;
    });
}

export const CURVE_PRESETS: readonly CurvePreset[] = [
  {
    id: "v",
    label: "V",
    description: "Buy dips and sell rallies, more the further price moves.",
    levels: shape((offset, index) =>
      offset < 0 ? ramp(index) : offset > 0 ? -ramp(index) : 0,
    ),
  },
  {
    id: "inverted-v",
    label: "Λ",
    description: "Follow the move: buy if it rises, sell if it falls.",
    levels: shape((offset, index) =>
      offset > 0 ? ramp(index) : offset < 0 ? -ramp(index) : 0,
    ),
  },
  {
    id: "l",
    label: "L",
    description:
      "Buy full size at any price below the reference, like a limit buy.",
    levels: shape((offset) => (offset < 0 ? 100 : 0)),
  },
  {
    id: "inverted-l",
    label: "⅃",
    description:
      "Sell full size at any price above the reference, like a limit sell.",
    levels: shape((offset) => (offset > 0 ? -100 : 0)),
  },
  {
    id: "slice-buy",
    label: "Slice buy",
    description: "A limit buy that grows: more size the lower it clears.",
    levels: shape((offset, index) => (offset < 0 ? ramp(index) : 0)),
  },
  {
    id: "slice-sell",
    label: "Slice sell",
    description: "A limit sell that grows: more size the higher it clears.",
    levels: shape((offset, index) => (offset > 0 ? -ramp(index) : 0)),
  },
];

/** The preset `levels` matches exactly, if any. */
export function matchingPreset(
  levels: readonly number[],
): CurvePreset | undefined {
  return CURVE_PRESETS.find((preset) =>
    preset.levels().every((value, index) => value === levels[index]),
  );
}
