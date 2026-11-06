import { describe, expect, it } from "vitest";

import {
  baseQuantityDecimals,
  baseTokensToAtoms,
  baseTokensToUnits,
  baseUnitsToTokens,
  describeAmounts,
  formatBase,
  formatBaseAtoms,
  formatPrice,
} from "./format";

const tbtc = {
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
};
const beat = {
  base: { symbol: "BEAT", decimals: 2 },
  quote: { symbol: "tUSD", decimals: 6 },
};

describe("formatPrice", () => {
  it("reads a tBTC price as whole tUSD", () => {
    expect(formatPrice(85_500n, tbtc)).toBe("85,500");
    expect(formatPrice(1_002_001n, tbtc)).toBe("1,002,001");
  });

  it("reads a BEAT price as tUSD per BEAT", () => {
    expect(formatPrice(1_002_001n, beat)).toBe("1.002001");
    expect(formatPrice(993_985n, beat)).toBe("0.993985");
    expect(formatPrice(1_010_017n, beat)).toBe("1.010017");
  });

  it("keeps every price on a market the same width", () => {
    expect(formatPrice(1_000_000n, beat)).toBe("1.000000");
    expect(formatPrice(1_200_000n, beat)).toBe("1.200000");
    expect(formatPrice(12_345_000_000n, beat)).toBe("12,345.000000");
  });
});

describe("base amounts", () => {
  it("allows whole base units only", () => {
    expect(baseQuantityDecimals(tbtc)).toBe(6);
    expect(baseQuantityDecimals(beat)).toBe(0);
  });

  it("treats a tBTC base unit as 100 satoshis", () => {
    expect(formatBase(1n, tbtc)).toBe("0.000001");
    expect(formatBaseAtoms(5_000_000n, tbtc)).toBe("0.05");
    expect(baseUnitsToTokens(100_000n, tbtc)).toBe("0.1");
    expect(baseTokensToUnits("0.1", tbtc)).toBe(100_000n);
    expect(baseTokensToAtoms("0.05", tbtc)).toBe(5_000_000n);
  });

  it("treats a BEAT base unit as one whole BEAT", () => {
    expect(formatBase(998n, beat)).toBe("998");
    expect(formatBase(1_500n, beat)).toBe("1,500");
    expect(formatBaseAtoms(12_345n, beat)).toBe("123.45");
    expect(baseUnitsToTokens(998n, beat)).toBe("998");
    expect(baseTokensToUnits("998", beat)).toBe(998n);
    expect(baseTokensToAtoms("1.5", beat)).toBe(150n);
  });

  it("rejects a fraction of a base unit", () => {
    expect(() => baseTokensToUnits("0.00000001", tbtc)).toThrow(/multiple/);
    expect(() => baseTokensToUnits("0.5", beat)).toThrow(/multiple/);
    expect(() => baseTokensToUnits("0.001", beat)).toThrow(/decimal places/);
  });
});

describe("describeAmounts", () => {
  it("names each market's tokens at their own scale", () => {
    expect(describeAmounts(5_000_000n, 5_000_000_000n, tbtc)).toBe(
      "0.05 tBTC and 5,000 tUSD",
    );
    expect(describeAmounts(12_345n, 0n, beat)).toBe("123.45 BEAT");
  });
});
