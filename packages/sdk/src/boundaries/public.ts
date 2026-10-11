import type { BeatExProver, ProofExecutionOptions } from "./prover.js";
import type {
  Address,
  Cluster,
  ConfirmedTransaction,
  OrderCurve,
  ProofMetadata,
} from "./primitives.js";
import type {
  BeatExRpc,
  BeatExTransactionCompiler,
  TransactionMessageConfig,
} from "./rpc.js";
import type { BeatExSigner } from "./signer.js";
import type { PrivateStateRecord, PrivateStateStorage } from "./storage.js";

export interface BeatExClientConfig {
  readonly rpc: BeatExRpc;
  readonly signer: BeatExSigner;
  readonly prover: BeatExProver;
  readonly storage: PrivateStateStorage;
  readonly transactionCompiler: BeatExTransactionCompiler;
  readonly cluster: Cluster;
  readonly programId?: Address;
  readonly allowMainnet?: boolean;
  readonly transactionConfig?: Partial<Omit<TransactionMessageConfig, "version" | "maxTransactionBytes">>;
}

export interface MarketRequest {
  readonly market: Address;
  readonly proofOptions?: ProofExecutionOptions;
}

export interface DepositParams extends MarketRequest {
  readonly baseSource: Address;
  readonly quoteSource: Address;
  readonly baseAmount: bigint;
  readonly quoteAmount: bigint;
}

export interface PlaceOrderParams extends MarketRequest {
  readonly curve: OrderCurve;
  readonly clientOrderNonce?: bigint;
  /** Materialize the slot-derived auction when it does not exist. Default true. */
  readonly initializeAuction?: boolean;
}

export interface ReplaceOrderParams extends MarketRequest {
  readonly curve: OrderCurve;
  readonly clientOrderNonce?: bigint;
}

export interface CancelOrderParams extends MarketRequest {}

export interface CloseOrderEventsParams {
  readonly market: Address;
  readonly auction: Address;
  /**
   * This owner's queued-order events in `auction` to close: events superseded
   * by a replacement, or the events of a cancelled order. Settlement, refund
   * and rollover already close the latest event. The auction must be settled
   * or cancelled.
   */
  readonly events: readonly Address[];
}

export interface SettleToBalanceParams extends MarketRequest {
  readonly auctionId: bigint;
}

export interface RefundToBalanceParams extends MarketRequest {
  readonly auctionId: bigint;
}

export interface RolloverParams extends MarketRequest {
  readonly previousAuctionId: bigint;
  readonly nextCurve: OrderCurve;
  readonly clientOrderNonce?: bigint;
  readonly initializeAuction?: boolean;
}

export interface WithdrawParams extends MarketRequest {
  readonly baseDestination: Address;
  readonly quoteDestination: Address;
  readonly baseAmount: bigint;
  readonly quoteAmount: bigint;
}

export interface TransactionResult {
  readonly transaction: ConfirmedTransaction;
  readonly privateState: PrivateStateRecord;
  readonly proof?: ProofMetadata;
}

export interface DepositResult extends TransactionResult {
  readonly privateBalance: Address;
  readonly initializationTransaction?: ConfirmedTransaction;
}

export interface OrderResult extends TransactionResult {
  readonly auction: Address;
  readonly auctionId: bigint;
  readonly activeOrder: Address;
  readonly queueEvent: Address;
  readonly auctionInitializationTransaction?: ConfirmedTransaction;
}

export interface SettlementResult extends TransactionResult {
  readonly auction: Address;
  readonly basePayout?: bigint;
  readonly quotePayout?: bigint;
}

export interface RolloverResult extends OrderResult {
  readonly previousAuction: Address;
  readonly basePayout: bigint;
  readonly quotePayout: bigint;
}

export interface WithdrawResult extends TransactionResult {
  readonly baseAmount: bigint;
  readonly quoteAmount: bigint;
}
