import type {
  Bytes32,
  Ciphertext,
  Groth16Proof,
  OrderCurve,
  PrivateBalanceOpening,
  ProofMetadata,
} from "./primitives.js";

export interface ProofCancellationSignal {
  readonly aborted: boolean;
  readonly reason?: unknown;
  addEventListener?(type: "abort", listener: () => void, options?: { once?: boolean }): void;
  removeEventListener?(type: "abort", listener: () => void): void;
}

export interface ProvingProgress {
  readonly phase: "loading" | "witness" | "proving" | "serializing";
  readonly completed?: number;
  readonly total?: number;
}

export interface ProofExecutionOptions {
  readonly signal?: ProofCancellationSignal;
  readonly onProgress?: (progress: ProvingProgress) => void;
}

export interface BalanceProofContext {
  readonly market: Bytes32;
  readonly owner: Bytes32;
  readonly keyEpoch: Bytes32;
  readonly committeeKey: Bytes32;
}

export interface DepositProofRequest extends BalanceProofContext {
  readonly oldRevision: bigint;
  readonly baseAmount: bigint;
  readonly quoteAmount: bigint;
  readonly oldBalance: PrivateBalanceOpening;
}

export interface DepositProofResult extends ProofMetadata {
  readonly proof: Groth16Proof;
  readonly newEncryptedBase: Ciphertext;
  readonly newEncryptedQuote: Ciphertext;
  readonly newBalance: PrivateBalanceOpening;
}

export interface OrderProofRequest {
  readonly auction: Bytes32;
  readonly owner: Bytes32;
  readonly clientOrderNonce: bigint;
  readonly referencePrice: bigint;
  readonly tickSize: bigint;
  readonly committeeKey: Bytes32;
  readonly curve: OrderCurve;
  readonly oldBalance: PrivateBalanceOpening;
}

export interface OrderProofResult extends ProofMetadata {
  readonly proof: Groth16Proof;
  readonly orderCommitment: Bytes32;
  /** Canonical interleaved order: buy[0], sell[0], buy[1], sell[1], ... */
  readonly ciphertexts: readonly Ciphertext[];
  readonly newEncryptedBase: Ciphertext;
  readonly newEncryptedQuote: Ciphertext;
  readonly newBalance: PrivateBalanceOpening;
  readonly orderSalt: Bytes32;
  readonly baseReserve: bigint;
  readonly quoteReserve: bigint;
}

export interface CollateralReleaseProofRequest {
  readonly market: Bytes32;
  readonly auction: Bytes32;
  readonly activeOrder: Bytes32;
  readonly owner: Bytes32;
  readonly balanceRevision: bigint;
  readonly clientOrderNonce: bigint;
  readonly curve: OrderCurve;
  readonly baseReserve: bigint;
  readonly quoteReserve: bigint;
  readonly orderSalt: Bytes32;
  readonly expectedOrderCommitment: Bytes32;
  readonly committeeKey: Bytes32;
  readonly oldBalance: PrivateBalanceOpening;
}

export interface CollateralReleaseProofResult extends ProofMetadata {
  readonly proof: Groth16Proof;
  readonly releasedEncryptedBase: Ciphertext;
  readonly releasedEncryptedQuote: Ciphertext;
  readonly releasedBalance: PrivateBalanceOpening;
}

export interface ReplaceOrderProofRequest {
  readonly release: CollateralReleaseProofRequest;
  /** oldBalance is supplied by the release proof and cannot be chosen separately. */
  readonly replacement: Omit<OrderProofRequest, "oldBalance">;
}

export interface ReplaceOrderProofResult extends ProofMetadata {
  readonly release: CollateralReleaseProofResult;
  readonly replacement: OrderProofResult;
}

export type CancelOrderProofRequest = CollateralReleaseProofRequest;
export type CancelOrderProofResult = CollateralReleaseProofResult;

export interface SettlementProofRequest {
  readonly market: Bytes32;
  readonly auction: Bytes32;
  readonly order: Bytes32;
  readonly activeOrder: Bytes32;
  readonly owner: Bytes32;
  readonly keyEpoch: Bytes32;
  readonly clientOrderNonce: bigint;
  readonly auctionSequence: bigint;
  readonly allocationRuleVersion: number;
  readonly orderCircuitVersion: number;
  readonly claimCircuitVersion: number;
  readonly curve: OrderCurve;
  readonly baseReserve: bigint;
  readonly quoteReserve: bigint;
  readonly orderSalt: Bytes32;
  readonly expectedOrderCommitment: Bytes32;
  readonly clearingIndex: number;
  readonly clearingPrice: bigint;
  readonly demandAtClearing: bigint;
  readonly supplyAtClearing: bigint;
  readonly clearingVolume: bigint;
  readonly balanceRevision: bigint;
  readonly committeeKey: Bytes32;
  readonly oldBalance: PrivateBalanceOpening;
}

export interface SettlementProofResult extends ProofMetadata {
  readonly proof: Groth16Proof;
  readonly terminalNullifier: Bytes32;
  readonly creditedEncryptedBase: Ciphertext;
  readonly creditedEncryptedQuote: Ciphertext;
  readonly creditedBalance: PrivateBalanceOpening;
  readonly basePayout: bigint;
  readonly quotePayout: bigint;
}

export interface RolloverProofRequest {
  readonly settlement: SettlementProofRequest;
  /** The settlement's credited balance is the mandatory input to this order proof. */
  readonly nextOrder: Omit<OrderProofRequest, "oldBalance">;
}

export interface RolloverProofResult extends ProofMetadata {
  readonly settlement: SettlementProofResult;
  readonly nextOrder: OrderProofResult;
}

export interface WithdrawProofRequest extends BalanceProofContext {
  readonly currentRevision: bigint;
  readonly baseAmount: bigint;
  readonly quoteAmount: bigint;
  readonly currentBalance: PrivateBalanceOpening;
}

export interface WithdrawProofResult extends ProofMetadata {
  readonly proof: Groth16Proof;
  readonly remainingEncryptedBase: Ciphertext;
  readonly remainingEncryptedQuote: Ciphertext;
  readonly remainingBalance: PrivateBalanceOpening;
}

/** Pluggable proof engine: browser WASM, worker, native bridge, or remote service. */
export interface BeatExProver {
  proveDeposit(
    request: DepositProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<DepositProofResult>;
  proveOrder(
    request: OrderProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<OrderProofResult>;
  proveReplacement(
    request: ReplaceOrderProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<ReplaceOrderProofResult>;
  proveCancellation(
    request: CancelOrderProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<CancelOrderProofResult>;
  proveSettlement(
    request: SettlementProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<SettlementProofResult>;
  proveRollover(
    request: RolloverProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<RolloverProofResult>;
  proveWithdrawal(
    request: WithdrawProofRequest,
    options?: ProofExecutionOptions,
  ): Promise<WithdrawProofResult>;
}
