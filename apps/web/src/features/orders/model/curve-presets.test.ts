import { PRICE_LEVELS } from "@beatex/sdk";
import { describe, expect, it } from "vitest";

import { CURVE_PRESETS, matchingPreset } from "./curve-presets";
import { CENTER_INDEX } from "./order-curve";

const byId = (id: string) => CURVE_PRESETS.find((preset) => preset.id === id)!;

describe("curve presets", () => {
  it.each(CURVE_PRESETS.map((preset) => [preset.id, preset] as const))(
    "%s is a valid curve",
    (_, preset) => {
      const levels = preset.levels();
      expect(levels).toHaveLength(PRICE_LEVELS);
      expect(levels.every((value) => value >= -100 && value <= 100)).toBe(true);
      // The reference level never trades.
      expect(levels[CENTER_INDEX]).toBe(0);
    },
  );

  it("V buys below and sells above, largest at the edges", () => {
    const levels = byId("v").levels();
    expect(levels[0]).toBe(100);
    expect(levels[CENTER_INDEX - 1]).toBe(13);
    expect(levels[PRICE_LEVELS - 1]).toBe(-100);
  });

  it("L is a full-size limit buy and its inverse a full-size limit sell", () => {
    expect(byId("l").levels().slice(0, CENTER_INDEX)).toEqual(
      Array(CENTER_INDEX).fill(100),
    );
    expect(
      byId("l")
        .levels()
        .slice(CENTER_INDEX + 1),
    ).toEqual(Array(CENTER_INDEX).fill(0));
    expect(
      byId("inverted-l")
        .levels()
        .slice(CENTER_INDEX + 1),
    ).toEqual(Array(CENTER_INDEX).fill(-100));
  });

  it("slices ramp up away from the reference on one side only", () => {
    const buy = byId("slice-buy").levels();
    expect(buy.slice(0, CENTER_INDEX)).toEqual(
      [...buy.slice(0, CENTER_INDEX)].sort((a, b) => b - a),
    );
    expect(buy.slice(CENTER_INDEX)).toEqual(Array(CENTER_INDEX + 1).fill(0));
    expect(Math.min(...byId("slice-sell").levels())).toBe(-100);
  });

  it("recognises a curve that matches a preset", () => {
    expect(matchingPreset(byId("l").levels())?.id).toBe("l");
    expect(matchingPreset(Array(PRICE_LEVELS).fill(7))).toBeUndefined();
  });
});
