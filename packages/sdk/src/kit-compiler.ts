import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  blockhash,
  compileTransaction,
  createTransactionMessage,
  getTransactionEncoder,
  pipe,
  setTransactionMessageComputeUnitLimit,
  setTransactionMessageComputeUnitPrice,
  setTransactionMessageConfig,
  setTransactionMessageFeePayer,
  setTransactionMessageHeapSize,
  setTransactionMessageLoadedAccountsDataSizeLimit,
  setTransactionMessageLifetimeUsingBlockhash,
  type Instruction,
} from "@solana/kit";
import type {
  BeatExTransactionCompiler,
  CompileTransactionRequest,
} from "./boundaries/rpc.js";

/**
 * Compile BeatEx transactions for any byte-oriented wallet signer: extended v1 by
 * default, or legacy with Compute Budget instructions when the config asks for it.
 */
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

    const lifetime = {
      blockhash: blockhash(request.recentBlockhash.blockhash),
      lastValidBlockHeight: request.recentBlockhash.lastValidBlockHeight,
    };
    const computeUnitLimit = request.config.computeUnitLimit ?? 1_400_000;
    const loadedAccountsDataSizeLimit =
      request.config.loadedAccountsDataSizeLimit ?? 2 * 1024 * 1024;
    const heapSize = request.config.heapSize ?? 256 * 1024;
    const priorityFeeLamports = request.config.priorityFeeLamports ?? 0n;

    const message =
      request.config.version === 1
        ? pipe(
            createTransactionMessage({ version: 1 }),
            (value) => setTransactionMessageFeePayer(address(request.feePayer), value),
            (value) => setTransactionMessageLifetimeUsingBlockhash(lifetime, value),
            (value) => appendTransactionMessageInstructions(instructions, value),
            (value) =>
              setTransactionMessageConfig(
                { computeUnitLimit, loadedAccountsDataSizeLimit, heapSize, priorityFeeLamports },
                value,
              ),
          )
        : pipe(
            createTransactionMessage({ version: "legacy" }),
            (value) => setTransactionMessageFeePayer(address(request.feePayer), value),
            (value) => setTransactionMessageLifetimeUsingBlockhash(lifetime, value),
            (value) => appendTransactionMessageInstructions(instructions, value),
            (value) => setTransactionMessageComputeUnitLimit(computeUnitLimit, value),
            (value) => setTransactionMessageLoadedAccountsDataSizeLimit(loadedAccountsDataSizeLimit, value),
            (value) => setTransactionMessageHeapSize(heapSize, value),
            // Legacy priority fees are priced per compute unit in micro-lamports.
            (value) =>
              priorityFeeLamports === 0n
                ? value
                : setTransactionMessageComputeUnitPrice(
                    (priorityFeeLamports * 1_000_000n) / BigInt(computeUnitLimit),
                    value,
                  ),
          );
    const transaction = compileTransaction(message);
    const encoded = new Uint8Array(getTransactionEncoder().encode(transaction));
    if (encoded.length > request.config.maxTransactionBytes) {
      throw new RangeError(
        `BeatEx ${request.config.version} transaction is ${encoded.length} bytes; maximum is ${request.config.maxTransactionBytes}`,
      );
    }
    return encoded;
  }
}

export function createKitTransactionCompiler(): BeatExTransactionCompiler {
  return new KitTransactionCompiler();
}
