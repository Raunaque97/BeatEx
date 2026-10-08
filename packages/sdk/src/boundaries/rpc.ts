import type {
  Address,
  ConfirmedTransaction,
  TransactionSignature,
} from "./primitives.js";

export type Commitment = "processed" | "confirmed" | "finalized";

export interface AccountInfo {
  readonly owner: Address;
  readonly lamports: bigint;
  readonly executable: boolean;
  readonly rentEpoch?: bigint;
  readonly data: Uint8Array;
}

export interface LatestBlockhash {
  readonly blockhash: string;
  readonly lastValidBlockHeight: bigint;
}

export interface SendTransactionOptions {
  readonly skipPreflight?: boolean;
  readonly preflightCommitment?: Commitment;
  readonly maxRetries?: number;
}

export interface ConfirmTransactionOptions {
  readonly commitment?: Exclude<Commitment, "processed">;
  readonly blockhash?: string;
  readonly lastValidBlockHeight?: bigint;
}

export interface SimulationResult {
  readonly err: unknown | null;
  readonly logs: readonly string[];
  readonly unitsConsumed?: bigint;
  readonly returnData?: Uint8Array;
}

export interface InstructionAccountMeta {
  readonly address: Address;
  readonly writable: boolean;
  readonly signer: boolean;
}

export interface SolanaInstruction {
  readonly programId: Address;
  readonly accounts: readonly InstructionAccountMeta[];
  readonly data: Uint8Array;
}

export interface TransactionMessageConfig {
  /** BeatEx uses Solana's extended v1 transaction format. */
  readonly version: 1;
  readonly maxTransactionBytes: 4096;
  readonly computeUnitLimit?: number;
  readonly loadedAccountsDataSizeLimit?: number;
  readonly heapSize?: number;
  readonly priorityFeeLamports?: bigint;
}

export interface CompileTransactionRequest {
  readonly feePayer: Address;
  readonly recentBlockhash: LatestBlockhash;
  /** Instruction order is consensus-relevant and must be preserved exactly. */
  readonly instructions: readonly SolanaInstruction[];
  readonly config: TransactionMessageConfig;
}

/** Adapter around the selected Solana transaction-message implementation. */
export interface BeatExTransactionCompiler {
  compileTransaction(request: CompileTransactionRequest): Promise<Uint8Array>;
}

/**
 * Minimal transport needed by BeatEx. Adapters may wrap web3.js, Kit, a wallet
 * RPC bridge, or a test transport without leaking those dependencies into the SDK.
 */
export interface BeatExRpc {
  getAccount(address: Address, commitment?: Commitment): Promise<AccountInfo | null>;
  getMultipleAccounts(
    addresses: readonly Address[],
    commitment?: Commitment,
  ): Promise<readonly (AccountInfo | null)[]>;
  getSlot(commitment?: Commitment): Promise<bigint>;
  getLatestBlockhash(commitment?: Commitment): Promise<LatestBlockhash>;
  simulateTransaction(transaction: Uint8Array): Promise<SimulationResult>;
  sendRawTransaction(
    signedTransaction: Uint8Array,
    options?: SendTransactionOptions,
  ): Promise<TransactionSignature>;
  confirmTransaction(
    signature: TransactionSignature,
    options?: ConfirmTransactionOptions,
  ): Promise<ConfirmedTransaction>;
}
