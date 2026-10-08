import {
  getTransactionDecoder,
  getTransactionEncoder,
  partiallySignTransactionWithSigners,
  type TransactionModifyingSigner,
  type TransactionPartialSigner,
} from "@solana/kit";
import type { BeatExSigner } from "./boundaries/signer.js";

/** Adapt an `@solana/kit` wallet or key-pair signer to the SDK's byte boundary. */
export function createKitSignerAdapter(
  signer: TransactionPartialSigner | TransactionModifyingSigner,
): BeatExSigner {
  return {
    address: signer.address,
    async signTransaction(bytes: Uint8Array): Promise<Uint8Array> {
      const transaction = getTransactionDecoder().decode(bytes);
      const signed = await partiallySignTransactionWithSigners([signer], transaction);
      return new Uint8Array(getTransactionEncoder().encode(signed));
    },
  };
}
