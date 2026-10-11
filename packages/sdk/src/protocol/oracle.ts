import { BorshReader } from "./bytes.js";
import { DIGEST_BYTES } from "./constants.js";

/** Anchor discriminator of the Pyth receiver's `PriceUpdateV2` account. */
export const PYTH_PRICE_UPDATE_V2_DISCRIMINATOR = Uint8Array.of(34, 241, 35, 99, 157, 126, 244, 205);

export interface PythPriceUpdate {
  readonly feedId: Uint8Array;
  readonly price: bigint;
  readonly conf: bigint;
  readonly exponent: number;
  readonly publishTime: bigint;
  readonly postedSlot: bigint;
}

/** Decode a Pyth receiver `PriceUpdateV2` account (the account a BeatEx market references). */
export function decodePythPriceUpdate(data: Uint8Array): PythPriceUpdate {
  if (data.length < 8 || !PYTH_PRICE_UPDATE_V2_DISCRIMINATOR.every((byte, index) => data[index] === byte)) {
    throw new Error("invalid Pyth PriceUpdateV2 account discriminator");
  }
  const reader = new BorshReader(data.subarray(8));
  reader.address(); // write authority
  const verificationLevel = reader.u8();
  if (verificationLevel === 0) reader.u8(); // Partial { num_signatures }
  else if (verificationLevel !== 1) throw new Error("invalid Pyth verification level");
  const feedId = reader.fixed(DIGEST_BYTES, "feedId");
  const price = reader.i64();
  const conf = reader.u64();
  const exponent = reader.u32() | 0;
  const publishTime = reader.i64();
  reader.i64(); // previous publish time
  reader.i64(); // EMA price
  reader.u64(); // EMA confidence
  const postedSlot = reader.u64();
  return { feedId, price, conf, exponent, publishTime, postedSlot };
}

/**
 * Mirror of the program's `normalize_oracle_price`: scale a positive Pyth price by
 * `exponent + exponentAdjustment`, rounding half up when the scale is negative.
 */
export function normalizeOraclePrice(price: bigint, exponent: number, exponentAdjustment: number): bigint {
  if (price <= 0n) throw new RangeError("oracle price must be positive");
  const adjusted = exponent + exponentAdjustment;
  const magnitude = Math.abs(adjusted);
  if (magnitude > 38) throw new RangeError("oracle exponent is out of range");
  const scale = 10n ** BigInt(magnitude);
  return adjusted >= 0 ? price * scale : (price + scale / 2n) / scale;
}
