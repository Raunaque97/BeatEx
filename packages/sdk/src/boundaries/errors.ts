export type BeatExErrorCode =
  | "ACCOUNT_NOT_FOUND"
  | "AUCTION_NOT_OPEN"
  | "AUCTION_NOT_SETTLED"
  | "INSUFFICIENT_PRIVATE_BALANCE"
  | "INVALID_ARGUMENT"
  | "INVALID_ACCOUNT_DATA"
  | "PROOF_CANCELLED"
  | "PROOF_FAILED"
  | "PRIVATE_STATE_MISSING"
  | "PRIVATE_STATE_CONFLICT"
  | "PRIVATE_STATE_TRANSITION_PENDING"
  | "PRIVATE_STATE_ATOMICITY_UNAVAILABLE"
  | "RPC_ERROR"
  | "SIMULATION_FAILED"
  | "TRANSACTION_TOO_LARGE"
  | "TRANSACTION_FAILED"
  | "WALLET_REJECTED";

export interface BeatExErrorOptions {
  readonly cause?: unknown;
  readonly retryable?: boolean;
  readonly context?: Readonly<Record<string, unknown>>;
}

export class BeatExError extends Error {
  readonly code: BeatExErrorCode;
  readonly retryable: boolean;
  readonly context: Readonly<Record<string, unknown>> | undefined;
  override readonly cause: unknown | undefined;

  constructor(code: BeatExErrorCode, message: string, options: BeatExErrorOptions = {}) {
    super(message);
    this.name = "BeatExError";
    this.code = code;
    this.retryable = options.retryable ?? false;
    this.context = options.context;
    this.cause = options.cause;
  }
}

export class PrivateStateConflictError extends BeatExError {
  constructor(message: string, context?: Readonly<Record<string, unknown>>) {
    super("PRIVATE_STATE_CONFLICT", message, {
      retryable: true,
      ...(context === undefined ? {} : { context }),
    });
    this.name = "PrivateStateConflictError";
  }
}

export function isBeatExError(error: unknown): error is BeatExError {
  return error instanceof BeatExError;
}
