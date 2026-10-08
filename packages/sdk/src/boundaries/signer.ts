import type { Address } from "./primitives.js";

export interface BeatExSigner {
  readonly address: Address;

  /**
   * Sign a fully compiled Solana transaction without submitting it.
   * Implementations may return a new array; callers must use the returned bytes.
   */
  signTransaction(transaction: Uint8Array): Promise<Uint8Array>;

  /** Optional wallet-native batch signing capability. */
  signAllTransactions?(transactions: readonly Uint8Array[]): Promise<readonly Uint8Array[]>;
}

export function isBatchSigner(
  signer: BeatExSigner,
): signer is BeatExSigner & Required<Pick<BeatExSigner, "signAllTransactions">> {
  return typeof signer.signAllTransactions === "function";
}
