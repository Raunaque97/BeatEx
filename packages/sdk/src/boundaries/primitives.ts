/** Dependency-free protocol primitives shared by every SDK boundary. */

declare const fixedBytesLength: unique symbol;

/** A byte array whose length has been checked at its trust boundary. */
export type FixedBytes<N extends number> = Uint8Array & {
  readonly [fixedBytesLength]: N;
};

export type Bytes32 = FixedBytes<32>;
export type Ciphertext = FixedBytes<64>;
export type ProofG1 = FixedBytes<64>;
export type ProofG2 = FixedBytes<128>;

/** Base58 Solana public key. It remains a string to avoid binding the SDK to web3.js. */
export type Address = string;
export type TransactionSignature = string;
export type Cluster = "devnet" | "mainnet-beta" | "localnet" | (string & {});

export const U64_MAX = (1n << 64n) - 1n;

export function asFixedBytes<const N extends number>(
  value: ArrayLike<number>,
  length: N,
  label = "byte array",
): FixedBytes<N> {
  if (value.length !== length) {
    throw new RangeError(`${label} must contain exactly ${length} bytes`);
  }
  return Uint8Array.from(value) as FixedBytes<N>;
}

export function asU64(value: bigint, label = "value"): bigint {
  if (value < 0n || value > U64_MAX) {
    throw new RangeError(`${label} must be an unsigned 64-bit integer`);
  }
  return value;
}

export interface EncryptedBalanceCiphertexts {
  readonly base: Ciphertext;
  readonly quote: Ciphertext;
}

export interface PrivateBalanceOpening {
  readonly baseValue: bigint;
  readonly quoteValue: bigint;
  readonly baseRandomness: Bytes32;
  readonly quoteRandomness: Bytes32;
}

/** Quantities are counts of base units, on both sides of the order. */
export interface OrderCurve {
  readonly buy: readonly bigint[];
  readonly sell: readonly bigint[];
}

export interface Groth16Proof {
  /** Negated G1 A point in Solana alt_bn128 big-endian encoding. */
  readonly proofA: ProofG1;
  /** G2 B point in Solana alt_bn128 big-endian encoding. */
  readonly proofB: ProofG2;
  /** G1 C point in Solana alt_bn128 big-endian encoding. */
  readonly proofC: ProofG1;
  /** Single canonical BN254 public input, big-endian. */
  readonly publicInput: Bytes32;
}

export interface ProofMetadata {
  readonly backend?: string;
  readonly provingTimeMs?: number;
  readonly circuitVersion?: number;
}

export interface ConfirmedTransaction {
  readonly signature: TransactionSignature;
  readonly slot: bigint;
  readonly confirmationStatus: "confirmed" | "finalized";
}
