/**
 * Clean typed or pasted text into a decimal amount: digits and one decimal
 * point, at most `decimals` fractional digits. A comma counts as the decimal
 * point for keyboards that type one.
 */
export function sanitizeDecimal(input: string, decimals: number): string {
  let value = input.replace(/,/g, ".").replace(/[^0-9.]/g, "");
  const point = value.indexOf(".");
  if (point !== -1) {
    const fraction = value
      .slice(point + 1)
      .replace(/\./g, "")
      .slice(0, decimals);
    value =
      decimals === 0
        ? value.slice(0, point)
        : `${value.slice(0, point)}.${fraction}`;
  }
  if (value.startsWith(".")) value = `0${value}`;
  // "007" -> "7", but keep "0" and "0.5".
  return value.replace(/^0+(?=\d)/, "");
}
