import { describe, expect, it } from "vitest";

import {
  DEMO_CLEARING,
  DEMO_DEMAND,
  DEMO_GRID,
  DEMO_SUPPLY,
  clearAuction,
} from "./clearing-demo";

describe("clearing demo", () => {
  it("matches the documented 85,500 grid", () => {
    expect(DEMO_GRID).toEqual([
      84816, 84902, 84987, 85073, 85158, 85244, 85329, 85415, 85500, 85586,
      85671, 85757, 85842, 85928, 86013, 86099, 86184,
    ]);
  });

  it("clears the story's curves one level above the reference", () => {
    expect(DEMO_CLEARING).toEqual({ index: 9, price: 85586, volume: 27 });
    expect(DEMO_DEMAND[DEMO_CLEARING.index]).toBe(36);
    expect(DEMO_SUPPLY[DEMO_CLEARING.index]).toBe(27);
  });

  it("breaks volume ties toward the previous price, then the lower price", () => {
    const grid = [100, 101, 102, 103];
    expect(clearAuction(grid, [5, 5, 5, 5], [5, 0, 0, 5], 102).index).toBe(3);
    expect(clearAuction(grid, [5, 0, 5, 0], [5, 0, 5, 0], 101).index).toBe(0);
  });

  it("picks the price nearest the tie-break when nothing matches", () => {
    expect(clearAuction([100, 101, 102], [0, 0, 0], [3, 3, 3], 101)).toEqual({
      index: 1,
      price: 101,
      volume: 0,
    });
  });
});
