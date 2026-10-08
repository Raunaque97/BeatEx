import { getAddressDecoder, type Address } from "@solana/kit";

const addressDecoder = getAddressDecoder();

export function fixedBytes(
  value: Uint8Array,
  length: number,
  name = "value",
): Uint8Array {
  if (value.length !== length) {
    throw new RangeError(`${name} must contain exactly ${length} bytes`);
  }
  return value;
}

export function u16Bytes(value: number): Uint8Array {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff) {
    throw new RangeError("u16 value is out of range");
  }
  const bytes = new Uint8Array(2);
  new DataView(bytes.buffer).setUint16(0, value, true);
  return bytes;
}

export function u64Bytes(value: bigint): Uint8Array {
  if (value < 0n || value > 0xffff_ffff_ffff_ffffn) {
    throw new RangeError("u64 value is out of range");
  }
  const bytes = new Uint8Array(8);
  new DataView(bytes.buffer).setBigUint64(0, value, true);
  return bytes;
}

export class BorshWriter {
  readonly #parts: Uint8Array[] = [];

  u8(value: number): this {
    if (!Number.isSafeInteger(value) || value < 0 || value > 0xff) {
      throw new RangeError("u8 value is out of range");
    }
    this.#parts.push(Uint8Array.of(value));
    return this;
  }

  bool(value: boolean): this {
    return this.u8(value ? 1 : 0);
  }

  u16(value: number): this {
    this.#parts.push(u16Bytes(value));
    return this;
  }

  u32(value: number): this {
    if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff_ffff) {
      throw new RangeError("u32 value is out of range");
    }
    const bytes = new Uint8Array(4);
    new DataView(bytes.buffer).setUint32(0, value, true);
    this.#parts.push(bytes);
    return this;
  }

  u64(value: bigint): this {
    this.#parts.push(u64Bytes(value));
    return this;
  }

  fixed(value: Uint8Array, length: number, name?: string): this {
    this.#parts.push(fixedBytes(value, length, name));
    return this;
  }

  bytes(value: Uint8Array): this {
    this.u32(value.length);
    this.#parts.push(value);
    return this;
  }

  append(value: Uint8Array): this {
    this.#parts.push(value);
    return this;
  }

  finish(): Uint8Array {
    const length = this.#parts.reduce((sum, part) => sum + part.length, 0);
    const output = new Uint8Array(length);
    let offset = 0;
    for (const part of this.#parts) {
      output.set(part, offset);
      offset += part.length;
    }
    return output;
  }
}

export class BorshReader {
  #offset = 0;

  constructor(readonly data: Uint8Array) {}

  get offset(): number {
    return this.#offset;
  }

  get remaining(): number {
    return this.data.length - this.#offset;
  }

  fixed(length: number, name = "field"): Uint8Array {
    const end = this.#offset + length;
    if (!Number.isSafeInteger(length) || length < 0 || end > this.data.length) {
      throw new RangeError(`account data ended while reading ${name}`);
    }
    const bytes = this.data.slice(this.#offset, end);
    this.#offset = end;
    return bytes;
  }

  u8(): number {
    return this.fixed(1, "u8")[0]!;
  }

  bool(): boolean {
    const value = this.u8();
    if (value !== 0 && value !== 1) {
      throw new Error(`invalid Borsh boolean: ${value}`);
    }
    return value === 1;
  }

  u16(): number {
    const bytes = this.fixed(2, "u16");
    return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint16(
      0,
      true,
    );
  }

  u32(): number {
    const bytes = this.fixed(4, "u32");
    return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(
      0,
      true,
    );
  }

  u64(): bigint {
    const bytes = this.fixed(8, "u64");
    return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getBigUint64(
      0,
      true,
    );
  }

  i64(): bigint {
    const bytes = this.fixed(8, "i64");
    return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getBigInt64(
      0,
      true,
    );
  }

  u128(): bigint {
    const low = this.u64();
    const high = this.u64();
    return low | (high << 64n);
  }

  bytes(name = "vector"): Uint8Array {
    return this.fixed(this.u32(), name);
  }

  address(): Address {
    return addressDecoder.decode(this.fixed(32, "address"));
  }
}

export function bytesEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}
