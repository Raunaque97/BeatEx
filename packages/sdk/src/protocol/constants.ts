import { address, type Address } from "@solana/kit";
import { GENERATED_PROGRAM_ID } from "./generated.js";

export const BEATEX_PROGRAM_ADDRESS = address(
  GENERATED_PROGRAM_ID,
);
export const TOKEN_2022_PROGRAM_ADDRESS = address(
  "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
);
export const SYSTEM_PROGRAM_ADDRESS = address(
  "11111111111111111111111111111111",
);
export const INSTRUCTIONS_SYSVAR_ADDRESS = address(
  "Sysvar1nstructions1111111111111111111111111",
);

export const PROTOCOL_VERSION = 2;
export const PRICE_LEVELS = 17;
export const BASE_UNIT_SIZE = 100n;
export const QUOTE_UNIT_SIZE = 1n;
export const GRID_STEP_BPS = 10;
export const CIPHERTEXT_BYTES = 64;
export const ORDER_CIPHERTEXT_COUNT = PRICE_LEVELS * 2;
export const ORDER_CIPHERTEXT_BYTES = ORDER_CIPHERTEXT_COUNT * CIPHERTEXT_BYTES;
export const DIGEST_BYTES = 32;
export const ORDER_SUBMISSION_CIRCUIT_VERSION = 2;
export const DEVNET_FAST_ORDER_CIRCUIT_VERSION = 3;
export const BALANCE_DEPOSIT_CIRCUIT_VERSION = 1;
export const AGGREGATE_FOLD_CIRCUIT_VERSION = 3;
export const AGGREGATE_PUBLICATION_CIRCUIT_VERSION = 3;
export const ORDER_MODIFICATION_CIRCUIT_VERSION = 2;
export const ROLLOVER_SETTLEMENT_CIRCUIT_VERSION = 3;
export const PRIVATE_CLAIM_CIRCUIT_VERSION = 3;
export const CANCELLATION_REFUND_CIRCUIT_VERSION = 3;

export const ZERO_ADDRESS = address(
  "11111111111111111111111111111111",
) as Address;

export enum AuctionState {
  Pending = 0,
  Settled = 1,
  Cancelled = 2,
}

export enum QueuedOrderState {
  Accepted = 0,
  Folded = 1,
}

export enum WithdrawalState {
  Pending = 0,
  Claimed = 1,
  Refunded = 2,
}

/** Discriminants are part of the on-chain verification-key PDA namespace. */
export enum QueueCircuitKind {
  OrderSubmission = 0,
  AggregateFold = 1,
  BalanceDeposit = 2,
  AggregatePublication = 3,
  PrivateClaim = 4,
  CancellationRefund = 5,
  OrderModification = 6,
  RolloverSettlement = 7,
}

export function circuitVersion(kind: QueueCircuitKind): number {
  switch (kind) {
    case QueueCircuitKind.AggregateFold:
      return AGGREGATE_FOLD_CIRCUIT_VERSION;
    case QueueCircuitKind.AggregatePublication:
      return AGGREGATE_PUBLICATION_CIRCUIT_VERSION;
    case QueueCircuitKind.OrderModification:
      return ORDER_MODIFICATION_CIRCUIT_VERSION;
    case QueueCircuitKind.RolloverSettlement:
      return ROLLOVER_SETTLEMENT_CIRCUIT_VERSION;
    case QueueCircuitKind.BalanceDeposit:
      return BALANCE_DEPOSIT_CIRCUIT_VERSION;
    case QueueCircuitKind.PrivateClaim:
      return PRIVATE_CLAIM_CIRCUIT_VERSION;
    case QueueCircuitKind.CancellationRefund:
      return CANCELLATION_REFUND_CIRCUIT_VERSION;
    case QueueCircuitKind.OrderSubmission:
      return ORDER_SUBMISSION_CIRCUIT_VERSION;
  }
}
