import { describe, expect, it } from "vitest";

import { addHoldings, valueInQuote } from "./portfolio";

describe("portfolio value", () => {
  it("values tBTC at the market price in tUSD atoms", () => {
    // 0.05 tBTC at 83,655 tUSD plus 5,000 tUSD = 9,182.75 tUSD.
    expect(
      valueInQuote(
        { baseAtoms: 5_000_000n, quoteAtoms: 5_000_000_000n },
        83_655n,
      ),
    ).toBe(9_182_750_000n);
  });

  it("adds an open order's locked reserves to the free balance", () => {
    expect(
      addHoldings(
        { baseAtoms: 1n, quoteAtoms: 2n },
        { baseAtoms: 10n, quoteAtoms: 20n },
      ),
    ).toEqual({ baseAtoms: 11n, quoteAtoms: 22n });
  });
});
