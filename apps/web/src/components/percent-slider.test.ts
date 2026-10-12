import { describe, expect, it } from "vitest";

import { percentOf, shareOf } from "./percent-slider";

describe("percent slider math", () => {
  it("takes a rounded-down share of a balance", () => {
    expect(shareOf(4_900_000_000n, 50)).toBe(2_450_000_000n);
    expect(shareOf(7n, 50)).toBe(3n);
    expect(shareOf(7n, 100)).toBe(7n);
  });

  it("reports an amount as a whole percent of the balance, capped at 100", () => {
    expect(percentOf(2_450_000_000n, 4_900_000_000n)).toBe(50);
    expect(percentOf(6n, 5n)).toBe(100);
    expect(percentOf(0n, 5n)).toBe(0);
    expect(percentOf(5n, 0n)).toBe(0);
  });
});
