import { describe, expect, it } from "vitest";

import { sanitizeDecimal } from "./decimal-input";

describe("sanitizeDecimal", () => {
  it.each([
    ["abc1.5x", 8, "1.5"],
    ["1.2.3", 8, "1.23"],
    ["0..5", 8, "0.5"],
    ["0,05", 8, "0.05"],
    [".5", 8, "0.5"],
    ["007", 8, "7"],
    ["0", 8, "0"],
    ["0.123456789", 8, "0.12345678"],
    ["5000.1234567", 6, "5000.123456"],
    ["1.", 6, "1."],
    ["1e5", 6, "15"],
    ["-3", 6, "3"],
  ])("%s with %i decimals -> %s", (input, decimals, expected) => {
    expect(sanitizeDecimal(input, decimals)).toBe(expected);
  });
});
