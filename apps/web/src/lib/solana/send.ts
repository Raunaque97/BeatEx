import type { BeatExRpc } from "@beatex/sdk";
import {
  appendTransactionMessageInstructions,
  createTransactionMessage,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  type Blockhash,
  type Instruction,
  type KeyPairSigner,
} from "@solana/kit";

/** Sign `instructions` with `payer`, send them, and wait for confirmation. */
export async function sendInstructions(
  rpc: BeatExRpc,
  payer: KeyPairSigner,
  instructions: readonly Instruction[],
): Promise<string> {
  const { blockhash, lastValidBlockHeight } =
    await rpc.getLatestBlockhash("confirmed");
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    (draft) => setTransactionMessageFeePayerSigner(payer, draft),
    (draft) =>
      setTransactionMessageLifetimeUsingBlockhash(
        { blockhash: blockhash as Blockhash, lastValidBlockHeight },
        draft,
      ),
    (draft) => appendTransactionMessageInstructions(instructions, draft),
  );
  const signed = await signTransactionMessageWithSigners(message);
  const bytes = new Uint8Array(getTransactionEncoder().encode(signed));
  const signature = await rpc.sendRawTransaction(bytes);
  await rpc.confirmTransaction(signature, {
    commitment: "confirmed",
    blockhash,
    lastValidBlockHeight,
  });
  return signature;
}
