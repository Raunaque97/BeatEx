import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  blockhash,
  compileTransaction,
  createTransactionMessage,
  getTransactionEncoder,
  pipe,
  setTransactionMessageConfig,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  type Instruction,
} from "@solana/kit";
import type {
  BeatExTransactionCompiler,
  CompileTransactionRequest,
} from "./boundaries/rpc.js";

/** Compile BeatEx's extended Solana v1 transactions for any byte-oriented wallet signer. */
export class KitTransactionCompiler implements BeatExTransactionCompiler {
  async compileTransaction(request: CompileTransactionRequest): Promise<Uint8Array> {
    const instructions: Instruction[] = request.instructions.map((instruction) => ({
      programAddress: address(instruction.programId),
      accounts: instruction.accounts.map((meta) => ({
        address: address(meta.address),
        role: meta.writable
          ? meta.signer
            ? AccountRole.WRITABLE_SIGNER
            : AccountRole.WRITABLE
          : meta.signer
            ? AccountRole.READONLY_SIGNER
            : AccountRole.READONLY,
      })),
      data: instruction.data,
    }));

    const message = pipe(
      createTransactionMessage({ version: 1 }),
      (value) => setTransactionMessageFeePayer(address(request.feePayer), value),
      (value) =>
        setTransactionMessageLifetimeUsingBlockhash(
          {
            blockhash: blockhash(request.recentBlockhash.blockhash),
            lastValidBlockHeight: request.recentBlockhash.lastValidBlockHeight,
          },
          value,
        ),
      (value) => appendTransactionMessageInstructions(instructions, value),
      (value) =>
        setTransactionMessageConfig(
          {
            computeUnitLimit: request.config.computeUnitLimit ?? 1_400_000,
            loadedAccountsDataSizeLimit:
              request.config.loadedAccountsDataSizeLimit ?? 2 * 1024 * 1024,
            heapSize: request.config.heapSize ?? 256 * 1024,
            priorityFeeLamports: request.config.priorityFeeLamports ?? 0n,
          },
          value,
        ),
    );
    const transaction = compileTransaction(message);
    const encoded = new Uint8Array(getTransactionEncoder().encode(transaction));
    if (encoded.length > request.config.maxTransactionBytes) {
      throw new RangeError(
        `BeatEx v1 transaction is ${encoded.length} bytes; maximum is ${request.config.maxTransactionBytes}`,
      );
    }
    return encoded;
  }
}

export function createKitTransactionCompiler(): BeatExTransactionCompiler {
  return new KitTransactionCompiler();
}
