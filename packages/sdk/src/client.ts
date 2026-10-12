import {
  getAddressEncoder,
  getSignatureFromTransaction,
  getTransactionDecoder,
  type Address as KitAddress,
} from "@solana/kit";
import {
  BeatExError,
  type BeatExClientConfig,
  type CancelOrderParams,
  type CloseOrderEventsParams,
  type ConfirmedTransaction,
  type DepositParams,
  type DepositResult,
  type OrderCurve,
  type OrderResult,
  type PlaceOrderParams,
  type PrivateStateKey,
  type PrivateStateOperation,
  type PrivateStateRecord,
  type RefundToBalanceParams,
  type ReplaceOrderParams,
  type RolloverParams,
  type RolloverResult,
  type SettleToBalanceParams,
  type SettlementResult,
  type SolanaInstruction,
  type StoredActiveOrder,
  type TraderPrivateState,
  type TransactionResult,
  type TransactionMessageConfig,
  type WithdrawParams,
  type WithdrawResult,
  type MoveBetweenMarketsParams,
  type MoveBetweenMarketsResult,
  asFixedBytes,
  asU64,
} from "./boundaries/index.js";
import {
  AuctionState,
  BEATEX_PROGRAM_ADDRESS,
  CIPHERTEXT_BYTES,
  INSTRUCTIONS_SYSVAR_ADDRESS,
  PRICE_LEVELS,
  QueueCircuitKind,
  QueuedOrderState,
  SYSTEM_PROGRAM_ADDRESS,
  TOKEN_2022_PROGRAM_ADDRESS,
  WithdrawalState,
  ZERO_ADDRESS,
  bytesEqual,
  decodeActiveOrder,
  decodeAggregateAccumulator,
  decodeAuction,
  decodeAnyAuction,
  isAuctionResult,
  clearingQuantities,
  decodeEncryptedPrivateBalance,
  decodeKeyEpoch,
  decodeMarket,
  decodeQueuedOrder,
  decodeSubmissionCounter,
  deriveActiveOrderPda,
  deriveAggregateAccumulatorPda,
  deriveAuctionPda,
  derivePrivateBalancePda,
  deriveAssociatedToken2022Address,
  ASSOCIATED_TOKEN_PROGRAM_ADDRESS,
  deriveQueuedOrderPda,
  deriveRolloverOrderPda,
  deriveSubmissionCounterPda,
  deriveVerificationKeyPda,
  deriveVaultAuthorityPda,
  encodeBindRolloverOrder,
  encodeBindRolloverSettlement,
  encodeCancelActiveOrder,
  encodeCloseOrderEvents,
  encodeDepositCollateral,
  encodeInitializeAuction,
  encodeInitializePrivateBalance,
  encodePrepareRollover,
  encodeRefundOrderToBalance,
  encodeReplaceQueuedOrder,
  encodeRolloverOrder,
  encodeSettleOrderToBalance,
  encodeSubmitQueuedOrder,
  encodeVerifyRolloverProof,
  encodeWithdrawPublicBalance,
  circuitVersion,
  type ActiveOrderAccount,
  type AggregateAccumulatorAccount,
  type AuctionAccount,
  type AnyAuctionAccount,
  type EncryptedPrivateBalanceAccount,
  type KeyEpochAccount,
  type MarketAccount,
  type QueuedOrderAccount,
  type SubmissionCounterAccount,
} from "./protocol/index.js";
import { TransactionOutcomeError } from "./json-rpc.js";
import { createKitTransactionCompiler } from "./kit-compiler.js";
import { auctionIdAtSlot, deriveAuctionPhase, hasOracle, type AuctionPhase } from "./schedule.js";

const addressEncoder = getAddressEncoder();

/** A signed transaction staged this long ago can no longer land (its blockhash expired). */
const UNSIGNED_TRANSITION_EXPIRY_MS = 3 * 60_000;

/** The fee payer's signature, which identifies the transaction, or null if undecodable. */
function transactionSignature(signed: Uint8Array): string | null {
  try {
    return getSignatureFromTransaction(getTransactionDecoder().decode(signed));
  } catch {
    return null;
  }
}
const V1_TRANSACTION_CONFIG = {
  version: 1 as const,
  maxTransactionBytes: 4096 as const,
  computeUnitLimit: 1_400_000,
  loadedAccountsDataSizeLimit: 2 * 1024 * 1024,
  heapSize: 256 * 1024,
  priorityFeeLamports: 0n,
};

const LEGACY_TRANSACTION_CONFIG = {
  ...V1_TRANSACTION_CONFIG,
  version: "legacy" as const,
  maxTransactionBytes: 1232 as const,
};

function transactionConfig(
  version: TransactionMessageConfig["version"],
  overrides: BeatExClientConfig["transactionConfig"],
): TransactionMessageConfig {
  const envelope = version === "legacy" ? LEGACY_TRANSACTION_CONFIG : V1_TRANSACTION_CONFIG;
  return {
    ...envelope,
    ...overrides,
    // The selected wire envelope and its protocol limit cannot be overridden.
    version: envelope.version,
    maxTransactionBytes: envelope.maxTransactionBytes,
  };
}

export interface QueueProgress {
  readonly acceptedOrderCount: bigint;
  readonly foldedOrderCount: bigint;
  readonly foldRevision: bigint;
  readonly complete: boolean;
}

interface ActiveContext {
  readonly record: PrivateStateRecord;
  readonly stored: StoredActiveOrder;
  /** Pending while the order can still change; compacted once it settles or is cancelled. */
  readonly auction: AnyAuctionAccount;
  readonly activeOrder: ActiveOrderAccount;
  readonly queueEvent: QueuedOrderAccount;
  readonly marketState: MarketAccount;
  readonly keyEpoch: KeyEpochAccount;
  readonly privateBalance: string;
}

function meta(address: string, writable = false, signer = false) {
  return { address, writable, signer };
}

function concatCiphertexts(ciphertexts: readonly Uint8Array[]): Uint8Array {
  if (ciphertexts.length !== PRICE_LEVELS * 2) {
    throw new BeatExError(
      "INVALID_ARGUMENT",
      `prover returned ${ciphertexts.length} order ciphertexts; expected ${PRICE_LEVELS * 2}`,
    );
  }
  const flattened = new Uint8Array(ciphertexts.length * CIPHERTEXT_BYTES);
  ciphertexts.forEach((ciphertext, index) => {
    if (ciphertext.length !== CIPHERTEXT_BYTES) {
      throw new BeatExError("INVALID_ARGUMENT", `ciphertext ${index} has the wrong length`);
    }
    flattened.set(ciphertext, index * CIPHERTEXT_BYTES);
  });
  return flattened;
}

function reverse32(bytes: Uint8Array): Uint8Array {
  return Uint8Array.from(bytes).reverse();
}

function identityCiphertext() {
  const bytes = new Uint8Array(CIPHERTEXT_BYTES);
  bytes[0] = 1;
  bytes[32] = 1;
  return asFixedBytes(bytes, 64, "identity ciphertext");
}

function zero32() {
  return asFixedBytes(new Uint8Array(32), 32, "zero scalar");
}

function addressBytes(value: string) {
  return asFixedBytes(addressEncoder.encode(value as KitAddress), 32, "address");
}

function randomU64(): bigint {
  const bytes = new Uint8Array(8);
  globalThis.crypto.getRandomValues(bytes);
  return new DataView(bytes.buffer).getBigUint64(0, true);
}

function randomTransitionId(): string {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (value) => value.toString(16).padStart(2, "0")).join("");
}

function validateCurve(curve: OrderCurve): void {
  if (curve.buy.length !== PRICE_LEVELS || curve.sell.length !== PRICE_LEVELS) {
    throw new BeatExError("INVALID_ARGUMENT", `orders require exactly ${PRICE_LEVELS} levels`);
  }
  for (let index = 0; index < PRICE_LEVELS; index += 1) {
    const buy = asU64(curve.buy[index]!, `buy[${index}]`);
    const sell = asU64(curve.sell[index]!, `sell[${index}]`);
    if (buy !== 0n && sell !== 0n) {
      throw new BeatExError(
        "INVALID_ARGUMENT",
        `price level ${index} cannot contain both a buy and a sell`,
      );
    }
  }
}

function initialState(keyEpoch: string): TraderPrivateState {
  const identity = identityCiphertext();
  return {
    balanceRevision: 0n,
    keyEpoch,
    balance: {
      baseValue: 0n,
      quoteValue: 0n,
      baseRandomness: zero32(),
      quoteRandomness: zero32(),
    },
    encryptedBase: identity,
    encryptedQuote: identity,
    activeOrder: null,
  };
}

function nextState(
  current: TraderPrivateState,
  values: {
    readonly balance: TraderPrivateState["balance"];
    readonly encryptedBase: Uint8Array;
    readonly encryptedQuote: Uint8Array;
    readonly activeOrder: StoredActiveOrder | null;
  },
): TraderPrivateState {
  return {
    balanceRevision: current.balanceRevision + 1n,
    keyEpoch: current.keyEpoch,
    balance: {
      baseValue: asU64(values.balance.baseValue, "private base balance"),
      quoteValue: asU64(values.balance.quoteValue, "private quote balance"),
      baseRandomness: asFixedBytes(values.balance.baseRandomness, 32, "base randomness"),
      quoteRandomness: asFixedBytes(values.balance.quoteRandomness, 32, "quote randomness"),
    },
    encryptedBase: asFixedBytes(values.encryptedBase, 64, "encrypted base balance"),
    encryptedQuote: asFixedBytes(values.encryptedQuote, 64, "encrypted quote balance"),
    activeOrder: values.activeOrder,
  };
}

export class BeatExClient {
  readonly #config: BeatExClientConfig;
  readonly programId: string;

  constructor(config: BeatExClientConfig) {
    if (config.cluster === "mainnet-beta" && config.allowMainnet !== true) {
      throw new BeatExError(
        "INVALID_ARGUMENT",
        "mainnet is disabled for this unaudited release; pass allowMainnet only after an explicit review",
      );
    }
    this.#config = config;
    this.programId = config.programId ?? BEATEX_PROGRAM_ADDRESS;
  }

  get owner(): string {
    return this.#config.signer.address;
  }

  #stateKey(market: string): PrivateStateKey {
    return {
      cluster: this.#config.cluster,
      programId: this.programId,
      market,
      owner: this.owner,
    };
  }

  #instruction(accounts: SolanaInstruction["accounts"], data: Uint8Array): SolanaInstruction {
    return { programId: this.programId, accounts, data };
  }

  async #accountData(address: string): Promise<Uint8Array | null> {
    const account = await this.#config.rpc.getAccount(address);
    if (account === null) return null;
    if (account.owner !== this.programId) {
      throw new BeatExError("INVALID_ACCOUNT_DATA", `account ${address} has the wrong owner`);
    }
    return account.data;
  }

  async #required<T>(address: string, decode: (data: Uint8Array) => T, label: string): Promise<T> {
    const data = await this.#accountData(address);
    if (data === null) throw new BeatExError("ACCOUNT_NOT_FOUND", `${label} ${address} was not found`);
    return decode(data);
  }

  async getMarket(market: string): Promise<MarketAccount> {
    return this.#required(market, decodeMarket, "market");
  }

  /** The auction in its current form: pending, or compacted once settled or cancelled. */
  async getAuction(market: string, auctionId: bigint): Promise<AnyAuctionAccount | null> {
    const [auction] = await deriveAuctionPda(market as KitAddress, asU64(auctionId), this.programId as KitAddress);
    const data = await this.#accountData(auction);
    return data === null ? null : decodeAnyAuction(data);
  }

  async getCurrentAuctionId(market: string, slot?: bigint): Promise<bigint> {
    const marketState = await this.getMarket(market);
    const auctionId = auctionIdAtSlot(marketState, slot ?? (await this.#config.rpc.getSlot()));
    if (auctionId === null) {
      throw new BeatExError("AUCTION_NOT_OPEN", "the market has not reached its start slot");
    }
    return auctionId;
  }

  async getAuctionPhase(market: string, auctionId: bigint, slot?: bigint): Promise<AuctionPhase> {
    const auction = await this.getAuction(market, auctionId);
    if (auction === null) return "not-started";
    if (isAuctionResult(auction)) {
      return auction.state === AuctionState.Settled ? "settled" : "cancelled";
    }
    return deriveAuctionPhase(auction, slot ?? (await this.#config.rpc.getSlot()));
  }

  async getPrivateBalance(market: string, owner = this.owner): Promise<EncryptedPrivateBalanceAccount | null> {
    const [address] = await derivePrivateBalancePda(
      market as KitAddress,
      owner as KitAddress,
      this.programId as KitAddress,
    );
    const data = await this.#accountData(address);
    return data === null ? null : decodeEncryptedPrivateBalance(data);
  }

  async getActiveOrder(auction: string, owner = this.owner): Promise<ActiveOrderAccount | null> {
    const [address] = await deriveActiveOrderPda(
      auction as KitAddress,
      owner as KitAddress,
      this.programId as KitAddress,
    );
    const data = await this.#accountData(address);
    return data === null ? null : decodeActiveOrder(data);
  }

  async getQueuedOrder(address: string): Promise<QueuedOrderAccount | null> {
    const data = await this.#accountData(address);
    return data === null ? null : decodeQueuedOrder(data);
  }

  async getOrderHistory(
    startEvent: string,
    maximumEntries = 100,
  ): Promise<readonly QueuedOrderAccount[]> {
    if (!Number.isSafeInteger(maximumEntries) || maximumEntries < 1) {
      throw new BeatExError("INVALID_ARGUMENT", "maximumEntries must be a positive integer");
    }
    const history: QueuedOrderAccount[] = [];
    let cursor = startEvent;
    const visited = new Set<string>();
    while (cursor !== ZERO_ADDRESS && history.length < maximumEntries) {
      if (visited.has(cursor)) {
        throw new BeatExError("INVALID_ACCOUNT_DATA", "queued-order history contains a cycle");
      }
      visited.add(cursor);
      const order = await this.getQueuedOrder(cursor);
      if (order === null) {
        // Superseded events are closed once their auction ends; the remaining
        // on-chain history stops at the first reclaimed link.
        if (cursor !== startEvent) break;
        throw new BeatExError("ACCOUNT_NOT_FOUND", `queued order ${cursor} was not found`);
      }
      history.push(order);
      cursor = order.previousEvent;
    }
    return history;
  }

  async getQueueProgress(market: string, auctionId: bigint): Promise<QueueProgress> {
    const [auction] = await deriveAuctionPda(market as KitAddress, auctionId, this.programId as KitAddress);
    const [[counter], [accumulator]] = await Promise.all([
      deriveSubmissionCounterPda(auction, this.programId as KitAddress),
      deriveAggregateAccumulatorPda(auction, this.programId as KitAddress),
    ]);
    const [counterState, accumulatorState] = await Promise.all([
      this.#required<SubmissionCounterAccount>(counter, decodeSubmissionCounter, "submission counter"),
      this.#required<AggregateAccumulatorAccount>(
        accumulator,
        decodeAggregateAccumulator,
        "aggregate accumulator",
      ),
    ]);
    return {
      acceptedOrderCount: counterState.acceptedOrderCount,
      foldedOrderCount: accumulatorState.foldedOrderCount,
      foldRevision: accumulatorState.foldRevision,
      complete: counterState.acceptedOrderCount === accumulatorState.foldedOrderCount,
    };
  }

  async getPrivateState(market: string): Promise<PrivateStateRecord | null> {
    const snapshot = await this.#config.storage.load(this.#stateKey(market));
    if (snapshot.pending !== null) {
      throw new BeatExError(
        "PRIVATE_STATE_TRANSITION_PENDING",
        `private transition ${snapshot.pending.id} must be resolved before continuing`,
        { retryable: true, context: { transitionId: snapshot.pending.id } },
      );
    }
    if (snapshot.record === null) return null;
    await this.#assertSynchronized(market, snapshot.record.state);
    return snapshot.record;
  }

  async #assertSynchronized(market: string, local: TraderPrivateState): Promise<void> {
    const onchain = await this.getPrivateBalance(market);
    if (onchain === null) {
      throw new BeatExError("PRIVATE_STATE_CONFLICT", "local private state exists but its account is missing");
    }
    if (
      onchain.revision !== local.balanceRevision ||
      onchain.keyEpoch !== local.keyEpoch ||
      !bytesEqual(onchain.encryptedBase, local.encryptedBase) ||
      !bytesEqual(onchain.encryptedQuote, local.encryptedQuote)
    ) {
      throw new BeatExError(
        "PRIVATE_STATE_CONFLICT",
        "local private balance does not match the on-chain ciphertexts and revision",
      );
    }
  }

  async #simulate(transaction: Uint8Array): Promise<void> {
    const simulation = await this.#config.rpc.simulateTransaction(transaction);
    if (simulation.err !== null) {
      const programError = simulation.logs.find((line) =>
        line.includes("Error Message:") || line.includes("Program log: Error:"),
      ) ?? [...simulation.logs].reverse().find((line) => line.includes("failed:"));
      const detail = programError ?? JSON.stringify(simulation.err);
      throw new BeatExError("SIMULATION_FAILED", `BeatEx transaction simulation failed: ${detail}`, {
        cause: simulation.err,
        context: { logs: simulation.logs, unitsConsumed: simulation.unitsConsumed?.toString() },
      });
    }
  }

  async #send(instructions: readonly SolanaInstruction[]): Promise<ConfirmedTransaction> {
    const recentBlockhash = await this.#config.rpc.getLatestBlockhash("confirmed");
    const config = transactionConfig(1, this.#config.transactionConfig);
    const transaction = await this.#config.transactionCompiler.compileTransaction({
      feePayer: this.owner,
      recentBlockhash,
      instructions,
      config,
    });
    // The RPC adapter disables signature verification, so execution errors can
    // be surfaced before asking the wallet to decode or sign the transaction.
    await this.#simulate(transaction);
    const signed = await this.#config.signer.signTransaction(transaction);
    if (signed.length > config.maxTransactionBytes) {
      throw new BeatExError(
        "TRANSACTION_TOO_LARGE",
        `signed BeatEx transaction is ${signed.length} bytes; maximum is ${config.maxTransactionBytes}`,
      );
    }
    // Modifying wallet signers may change the message; validate their returned
    // bytes as well before broadcasting.
    await this.#simulate(signed);
    const signature = await this.#config.rpc.sendRawTransaction(signed, {
      preflightCommitment: "confirmed",
      maxRetries: 3,
    });
    return this.#config.rpc.confirmTransaction(signature, {
      commitment: "confirmed",
      blockhash: recentBlockhash.blockhash,
      lastValidBlockHeight: recentBlockhash.lastValidBlockHeight,
    });
  }

  async #transition(
    market: string,
    operation: PrivateStateOperation,
    currentRecord: PrivateStateRecord | null,
    next: TraderPrivateState,
    instructions: readonly SolanaInstruction[],
    transactionVersion: TransactionMessageConfig["version"] = 1,
  ): Promise<{ transaction: ConfirmedTransaction; privateState: PrivateStateRecord }> {
    const result = await this.#transitions(
      [{ market, operation, currentRecord, next }],
      instructions,
      transactionVersion,
    );
    return { transaction: result.transaction, privateState: result.privateStates[0]! };
  }

  /**
   * One transaction that changes one or more markets' private balances. Each
   * market's next state is staged under the same transaction signature, so the
   * journal commits or rolls back all of them together, and recovery resolves
   * each market from that one signature.
   */
  async #transitions(
    entries: readonly {
      readonly market: string;
      readonly operation: PrivateStateOperation;
      readonly currentRecord: PrivateStateRecord | null;
      readonly next: TraderPrivateState;
    }[],
    instructions: readonly SolanaInstruction[],
    transactionVersion: TransactionMessageConfig["version"] = 1,
  ): Promise<{ transaction: ConfirmedTransaction; privateStates: PrivateStateRecord[] }> {
    const recentBlockhash = await this.#config.rpc.getLatestBlockhash("confirmed");
    const config = transactionConfig(transactionVersion, this.#config.transactionConfig);
    const transaction = await this.#config.transactionCompiler.compileTransaction({
      feePayer: this.owner,
      recentBlockhash,
      instructions,
      config,
    });
    await this.#simulate(transaction);
    const signed = await this.#config.signer.signTransaction(transaction);
    if (signed.length > config.maxTransactionBytes) {
      throw new BeatExError(
        "TRANSACTION_TOO_LARGE",
        `signed BeatEx transaction is ${signed.length} bytes; maximum is ${config.maxTransactionBytes}`,
      );
    }
    await this.#simulate(signed);

    const staged: { key: PrivateStateKey; transitionId: string }[] = [];
    try {
      for (const entry of entries) {
        const key = this.#stateKey(entry.market);
        const transitionId = randomTransitionId();
        await this.#config.storage.stage(key, {
          id: transitionId,
          operation: entry.operation,
          expectedStorageRevision: entry.currentRecord?.storageRevision ?? null,
          nextState: entry.next,
        });
        staged.push({ key, transitionId });
      }
    } catch (error) {
      // Nothing was sent; release whatever was staged before the failure.
      for (const { key, transitionId } of staged) {
        await this.#config.storage.abort(key, transitionId);
      }
      throw error;
    }
    const transitionIds = staged.map(({ transitionId }) => transitionId);
    const markBroadcast = async (signature: string) => {
      for (const { key, transitionId } of staged) {
        await this.#config.storage.markBroadcast(
          key,
          transitionId,
          signature,
          recentBlockhash.blockhash,
          recentBlockhash.lastValidBlockHeight,
        );
      }
    };

    // Record the signature before sending, so an interrupted send or
    // confirmation can always be resolved later by looking the signature up.
    const localSignature = transactionSignature(signed);
    if (localSignature !== null) await markBroadcast(localSignature);
    try {
      const signature = await this.#config.rpc.sendRawTransaction(signed, {
        preflightCommitment: "confirmed",
        maxRetries: 3,
      });
      if (localSignature === null) await markBroadcast(signature);
      const confirmation = await this.#config.rpc.confirmTransaction(signature, {
        commitment: "confirmed",
        blockhash: recentBlockhash.blockhash,
        lastValidBlockHeight: recentBlockhash.lastValidBlockHeight,
      });
      const privateStates: PrivateStateRecord[] = [];
      for (const { key, transitionId } of staged) {
        privateStates.push(await this.#config.storage.commitConfirmed(key, transitionId, confirmation));
      }
      return { transaction: confirmation, privateStates };
    } catch (cause) {
      if (cause instanceof TransactionOutcomeError) {
        // It can never apply, so every current state is still correct.
        for (const { key, transitionId } of staged) {
          await this.#config.storage.abort(key, transitionId);
        }
        throw new BeatExError(
          "TRANSACTION_FAILED",
          cause.outcome === "failed"
            ? "the transaction failed on-chain; your balance is unchanged"
            : "the transaction expired before it landed; your balance is unchanged",
          { cause, retryable: true, context: { transitionIds } },
        );
      }
      throw new BeatExError(
        "TRANSACTION_FAILED",
        "transaction outcome is unresolved; the staged private transition was retained",
        { cause, retryable: true, context: { transitionIds } },
      );
    }
  }

  /** Whether the on-chain private balance already holds exactly `state`'s ciphertexts. */
  async #onchainMatches(market: string, state: TraderPrivateState): Promise<boolean> {
    const onchain = await this.getPrivateBalance(market);
    return (
      onchain !== null &&
      onchain.revision === state.balanceRevision &&
      onchain.keyEpoch === state.keyEpoch &&
      bytesEqual(onchain.encryptedBase, state.encryptedBase) &&
      bytesEqual(onchain.encryptedQuote, state.encryptedQuote)
    );
  }

  async #ensurePrivateBalance(
    market: string,
    marketState: MarketAccount,
    transactionVersion: TransactionMessageConfig["version"] = 1,
  ): Promise<{ record: PrivateStateRecord; initializationTransaction?: ConfirmedTransaction }> {
    const existing = await this.#config.storage.load(this.#stateKey(market));
    if (existing.pending !== null) {
      throw new BeatExError(
        "PRIVATE_STATE_TRANSITION_PENDING",
        `private transition ${existing.pending.id} is still pending`,
        { retryable: true },
      );
    }
    if (existing.record !== null) {
      await this.#assertSynchronized(market, existing.record.state);
      return { record: existing.record };
    }
    const [privateBalance] = await derivePrivateBalancePda(
      market as KitAddress,
      this.owner as KitAddress,
      this.programId as KitAddress,
    );
    if ((await this.#accountData(privateBalance)) !== null) {
      throw new BeatExError(
        "PRIVATE_STATE_MISSING",
        "the on-chain private balance exists but its unrecoverable local opening is missing",
      );
    }
    const state = initialState(marketState.currentKeyEpoch);
    const result = await this.#transition(
      market,
      "initialize",
      null,
      state,
      [
        this.#instruction(
          [
            meta(this.owner, true, true),
            meta(market),
            meta(marketState.currentKeyEpoch),
            meta(privateBalance, true),
            meta(SYSTEM_PROGRAM_ADDRESS),
          ],
          encodeInitializePrivateBalance(state.encryptedBase, state.encryptedQuote),
        ),
      ],
      transactionVersion,
    );
    return { record: result.privateState, initializationTransaction: result.transaction };
  }

  async initializeCurrentAuction(market: string): Promise<{
    auctionId: bigint;
    auction: string;
    transaction?: ConfirmedTransaction;
  }> {
    const marketState = await this.getMarket(market);
    const auctionId = await this.getCurrentAuctionId(market);
    const [auction] = await deriveAuctionPda(market as KitAddress, auctionId, this.programId as KitAddress);
    if ((await this.#accountData(auction)) !== null) return { auctionId, auction };
    const [[counter], [accumulator]] = await Promise.all([
      deriveSubmissionCounterPda(auction, this.programId as KitAddress),
      deriveAggregateAccumulatorPda(auction, this.programId as KitAddress),
    ]);
    const transaction = await this.#send([
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(market),
          meta(marketState.currentKeyEpoch),
          // Anchor reads the program id as the omitted oracle of an oracle-free market.
          meta(hasOracle(marketState) ? marketState.oraclePriceAccount : this.programId),
          meta(auction, true),
          meta(counter, true),
          meta(accumulator, true),
          meta(SYSTEM_PROGRAM_ADDRESS),
        ],
        encodeInitializeAuction(auctionId),
      ),
    ]);
    return { auctionId, auction, transaction };
  }

  async deposit(params: DepositParams): Promise<DepositResult> {
    asU64(params.baseAmount, "baseAmount");
    asU64(params.quoteAmount, "quoteAmount");
    if (params.baseAmount === 0n && params.quoteAmount === 0n) {
      throw new BeatExError("INVALID_ARGUMENT", "deposit amount cannot be zero for both assets");
    }
    const marketState = await this.getMarket(params.market);
    const initialized = await this.#ensurePrivateBalance(params.market, marketState, "legacy");
    const current = initialized.record.state;
    const keyEpoch = await this.#required<KeyEpochAccount>(
      marketState.currentKeyEpoch,
      decodeKeyEpoch,
      "key epoch",
    );
    const output = await this.#config.prover.proveDeposit(
      {
        market: addressBytes(params.market),
        owner: addressBytes(this.owner),
        keyEpoch: addressBytes(marketState.currentKeyEpoch),
        committeeKey: asFixedBytes(keyEpoch.babyjubEncryptionPubkey, 32),
        oldRevision: current.balanceRevision,
        baseAmount: params.baseAmount,
        quoteAmount: params.quoteAmount,
        oldBalance: current.balance,
      },
      params.proofOptions,
    );
    const [privateBalance] = await derivePrivateBalancePda(
      params.market as KitAddress,
      this.owner as KitAddress,
      this.programId as KitAddress,
    );
    const [depositKey] = await deriveVerificationKeyPda(
      params.market as KitAddress,
      QueueCircuitKind.BalanceDeposit,
      circuitVersion(QueueCircuitKind.BalanceDeposit),
      this.programId as KitAddress,
    );
    const [vaultAuthority] = await deriveVaultAuthorityPda(
      params.market as KitAddress,
      this.programId as KitAddress,
    );
    const next = nextState(current, {
      balance: output.newBalance,
      encryptedBase: output.newEncryptedBase,
      encryptedQuote: output.newEncryptedQuote,
      activeOrder: current.activeOrder,
    });
    const result = await this.#transition(
      params.market,
      "deposit",
      initialized.record,
      next,
      [
        this.#instruction(
          [
            meta(this.owner, true, true),
            meta(params.market),
            meta(marketState.currentKeyEpoch),
            meta(depositKey),
            meta(privateBalance, true),
            meta(marketState.baseMint),
            meta(marketState.quoteMint),
            meta(params.baseSource, true),
            meta(params.quoteSource, true),
            meta(marketState.baseVault, true),
            meta(marketState.quoteVault, true),
            meta(vaultAuthority),
            meta(TOKEN_2022_PROGRAM_ADDRESS),
          ],
          encodeDepositCollateral({
            baseAmount: params.baseAmount,
            quoteAmount: params.quoteAmount,
            newEncryptedBase: output.newEncryptedBase,
            newEncryptedQuote: output.newEncryptedQuote,
            proof: output.proof,
          }),
        ),
      ],
      "legacy",
    );
    return {
      ...result,
      privateBalance,
      ...(initialized.initializationTransaction === undefined
        ? {}
        : { initializationTransaction: initialized.initializationTransaction }),
      proof: output,
    };
  }

  async #currentAuction(
    market: string,
    initialize: boolean,
  ): Promise<{
    auctionId: bigint;
    auctionAddress: string;
    auction: AuctionAccount;
    initializationTransaction?: ConfirmedTransaction;
  }> {
    const auctionId = await this.getCurrentAuctionId(market);
    const [auctionAddress] = await deriveAuctionPda(
      market as KitAddress,
      auctionId,
      this.programId as KitAddress,
    );
    let auction = await this.getAuction(market, auctionId);
    let initializationTransaction: ConfirmedTransaction | undefined;
    if (auction === null && initialize) {
      const initialized = await this.initializeCurrentAuction(market);
      initializationTransaction = initialized.transaction;
      auction = await this.getAuction(market, auctionId);
    }
    if (auction === null) throw new BeatExError("AUCTION_NOT_OPEN", "current auction is not initialized");
    if (isAuctionResult(auction)) {
      throw new BeatExError("AUCTION_NOT_OPEN", "current auction has already finished");
    }
    return {
      auctionId,
      auctionAddress,
      auction,
      ...(initializationTransaction === undefined ? {} : { initializationTransaction }),
    };
  }

  async placeOrder(params: PlaceOrderParams): Promise<OrderResult> {
    validateCurve(params.curve);
    const marketState = await this.getMarket(params.market);
    const initialized = await this.#ensurePrivateBalance(params.market, marketState);
    if (initialized.record.state.activeOrder !== null) {
      throw new BeatExError("INVALID_ARGUMENT", "an active order exists; use replaceOrder or rollover");
    }
    const current = await this.#currentAuction(params.market, params.initializeAuction ?? true);
    const keyEpoch = await this.#required<KeyEpochAccount>(current.auction.keyEpoch, decodeKeyEpoch, "key epoch");
    const [counterAddress] = await deriveSubmissionCounterPda(
      current.auctionAddress as KitAddress,
      this.programId as KitAddress,
    );
    const nonce = params.clientOrderNonce ?? randomU64();
    const output = await this.#config.prover.proveOrder(
      {
        auction: addressBytes(current.auctionAddress),
        owner: addressBytes(this.owner),
        clientOrderNonce: nonce,
        referencePrice: current.auction.referencePrice,
        gridStepBps: current.auction.gridStepBps,
        committeeKey: asFixedBytes(keyEpoch.babyjubEncryptionPubkey, 32),
        curve: params.curve,
        oldBalance: initialized.record.state.balance,
      },
      params.proofOptions,
    );
    const [[privateBalance], [event], [activeOrder], [orderKey]] = await Promise.all([
      derivePrivateBalancePda(params.market as KitAddress, this.owner as KitAddress, this.programId as KitAddress),
      deriveQueuedOrderPda(current.auctionAddress as KitAddress, this.owner as KitAddress, nonce, this.programId as KitAddress),
      deriveActiveOrderPda(current.auctionAddress as KitAddress, this.owner as KitAddress, this.programId as KitAddress),
      deriveVerificationKeyPda(params.market as KitAddress, QueueCircuitKind.OrderSubmission, current.auction.orderCircuitVersion, this.programId as KitAddress),
    ]);
    const active: StoredActiveOrder = {
      auctionId: current.auctionId,
      auction: current.auctionAddress,
      activeOrder,
      queueEvent: event,
      clientOrderNonce: nonce,
      curve: params.curve,
      orderSalt: output.orderSalt,
      orderCommitment: output.orderCommitment,
      baseReserve: output.baseReserve,
      quoteReserve: output.quoteReserve,
    };
    const next = nextState(initialized.record.state, {
      balance: output.newBalance,
      encryptedBase: output.newEncryptedBase,
      encryptedQuote: output.newEncryptedQuote,
      activeOrder: active,
    });
    const result = await this.#transition(params.market, "place-order", initialized.record, next, [
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(params.market),
          meta(current.auctionAddress),
          meta(current.auction.keyEpoch),
          meta(orderKey),
          meta(counterAddress, true),
          meta(privateBalance, true),
          meta(event, true),
          meta(activeOrder, true),
          meta(SYSTEM_PROGRAM_ADDRESS),
        ],
        encodeSubmitQueuedOrder({
          clientOrderNonce: nonce,
          orderCommitment: output.orderCommitment,
          ciphertexts: concatCiphertexts(output.ciphertexts),
          newEncryptedBase: output.newEncryptedBase,
          newEncryptedQuote: output.newEncryptedQuote,
          proof: output.proof,
        }),
      ),
    ]);
    return {
      ...result,
      auction: current.auctionAddress,
      auctionId: current.auctionId,
      activeOrder,
      queueEvent: event,
      ...(current.initializationTransaction === undefined
        ? {}
        : { auctionInitializationTransaction: current.initializationTransaction }),
      proof: output,
    };
  }

  async #activeContext(market: string, expectedAuctionId?: bigint): Promise<ActiveContext> {
    const record = await this.getPrivateState(market);
    if (record === null) throw new BeatExError("PRIVATE_STATE_MISSING", "private balance is not initialized");
    const stored = record.state.activeOrder;
    if (stored === null) throw new BeatExError("INVALID_ARGUMENT", "there is no active order");
    if (expectedAuctionId !== undefined && stored.auctionId !== expectedAuctionId) {
      throw new BeatExError("INVALID_ARGUMENT", "the active order belongs to a different auction");
    }
    const [auction, activeOrder, queueEvent, marketState] = await Promise.all([
      this.#required<AnyAuctionAccount>(stored.auction, decodeAnyAuction, "auction"),
      this.#required<ActiveOrderAccount>(stored.activeOrder, decodeActiveOrder, "active order"),
      this.#required<QueuedOrderAccount>(stored.queueEvent, decodeQueuedOrder, "queued order"),
      this.getMarket(market),
    ]);
    if (!activeOrder.active || activeOrder.latestEvent !== stored.queueEvent) {
      throw new BeatExError("PRIVATE_STATE_CONFLICT", "the local active order is not the active on-chain revision");
    }
    if (queueEvent.orderCommitment.length !== 32 || !bytesEqual(queueEvent.orderCommitment, stored.orderCommitment)) {
      throw new BeatExError("PRIVATE_STATE_CONFLICT", "the local order commitment does not match the chain");
    }
    const keyEpoch = await this.#required<KeyEpochAccount>(auction.keyEpoch, decodeKeyEpoch, "key epoch");
    const [privateBalance] = await derivePrivateBalancePda(
      market as KitAddress,
      this.owner as KitAddress,
      this.programId as KitAddress,
    );
    return { record, stored, auction, activeOrder, queueEvent, marketState, keyEpoch, privateBalance };
  }

  #releaseRequest(context: ActiveContext) {
    return {
      market: addressBytes(context.queueEvent.market),
      auction: addressBytes(context.stored.auction),
      activeOrder: addressBytes(context.stored.activeOrder),
      owner: addressBytes(this.owner),
      balanceRevision: context.record.state.balanceRevision,
      clientOrderNonce: context.stored.clientOrderNonce,
      curve: context.stored.curve,
      baseReserve: context.stored.baseReserve,
      quoteReserve: context.stored.quoteReserve,
      orderSalt: context.stored.orderSalt,
      expectedOrderCommitment: context.stored.orderCommitment,
      committeeKey: asFixedBytes(context.keyEpoch.babyjubEncryptionPubkey, 32),
      oldBalance: context.record.state.balance,
    };
  }

  async replaceOrder(params: ReplaceOrderParams): Promise<OrderResult> {
    validateCurve(params.curve);
    const context = await this.#activeContext(params.market);
    if (context.auction.state !== AuctionState.Pending) {
      throw new BeatExError("AUCTION_NOT_OPEN", "the active order's auction is no longer pending");
    }
    const [counterAddress] = await deriveSubmissionCounterPda(
      context.stored.auction as KitAddress,
      this.programId as KitAddress,
    );
    const nonce = params.clientOrderNonce ?? randomU64();
    const output = await this.#config.prover.proveReplacement(
      {
        release: this.#releaseRequest(context),
        replacement: {
          auction: addressBytes(context.stored.auction),
          owner: addressBytes(this.owner),
          clientOrderNonce: nonce,
          referencePrice: context.auction.referencePrice,
          gridStepBps: context.auction.gridStepBps,
          committeeKey: asFixedBytes(context.keyEpoch.babyjubEncryptionPubkey, 32),
          curve: params.curve,
        },
      },
      params.proofOptions,
    );
    const [[event], [releaseKey], [orderKey]] = await Promise.all([
      deriveQueuedOrderPda(context.stored.auction as KitAddress, this.owner as KitAddress, nonce, this.programId as KitAddress),
      deriveVerificationKeyPda(params.market as KitAddress, QueueCircuitKind.OrderModification, circuitVersion(QueueCircuitKind.OrderModification), this.programId as KitAddress),
      deriveVerificationKeyPda(params.market as KitAddress, QueueCircuitKind.OrderSubmission, context.auction.orderCircuitVersion, this.programId as KitAddress),
    ]);
    const active: StoredActiveOrder = {
      auctionId: context.stored.auctionId,
      auction: context.stored.auction,
      activeOrder: context.stored.activeOrder,
      queueEvent: event,
      clientOrderNonce: nonce,
      curve: params.curve,
      orderSalt: output.replacement.orderSalt,
      orderCommitment: output.replacement.orderCommitment,
      baseReserve: output.replacement.baseReserve,
      quoteReserve: output.replacement.quoteReserve,
    };
    const next = nextState(context.record.state, {
      balance: output.replacement.newBalance,
      encryptedBase: output.replacement.newEncryptedBase,
      encryptedQuote: output.replacement.newEncryptedQuote,
      activeOrder: active,
    });
    const result = await this.#transition(params.market, "replace-order", context.record, next, [
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(params.market),
          meta(context.stored.auction),
          meta(context.auction.keyEpoch),
          meta(releaseKey),
          meta(counterAddress, true),
          meta(context.privateBalance, true),
          meta(context.stored.activeOrder, true),
          meta(context.stored.queueEvent),
          meta(orderKey),
          meta(event, true),
          meta(SYSTEM_PROGRAM_ADDRESS),
        ],
        encodeReplaceQueuedOrder({
          release: {
            releasedEncryptedBase: output.release.releasedEncryptedBase,
            releasedEncryptedQuote: output.release.releasedEncryptedQuote,
            proof: output.release.proof,
          },
          replacement: {
            clientOrderNonce: nonce,
            orderCommitment: output.replacement.orderCommitment,
            ciphertexts: concatCiphertexts(output.replacement.ciphertexts),
            newEncryptedBase: output.replacement.newEncryptedBase,
            newEncryptedQuote: output.replacement.newEncryptedQuote,
            proof: output.replacement.proof,
          },
        }),
      ),
    ]);
    return {
      ...result,
      auction: context.stored.auction,
      auctionId: context.stored.auctionId,
      activeOrder: context.stored.activeOrder,
      queueEvent: event,
      proof: output,
    };
  }

  async cancelOrder(params: CancelOrderParams): Promise<TransactionResult> {
    const context = await this.#activeContext(params.market);
    const [counterAddress] = await deriveSubmissionCounterPda(
      context.stored.auction as KitAddress,
      this.programId as KitAddress,
    );
    const nonce = randomU64();
    const output = await this.#config.prover.proveCancellation(
      this.#releaseRequest(context),
      params.proofOptions,
    );
    const [[event], [releaseKey]] = await Promise.all([
      deriveQueuedOrderPda(context.stored.auction as KitAddress, this.owner as KitAddress, nonce, this.programId as KitAddress),
      deriveVerificationKeyPda(params.market as KitAddress, QueueCircuitKind.OrderModification, circuitVersion(QueueCircuitKind.OrderModification), this.programId as KitAddress),
    ]);
    const next = nextState(context.record.state, {
      balance: output.releasedBalance,
      encryptedBase: output.releasedEncryptedBase,
      encryptedQuote: output.releasedEncryptedQuote,
      activeOrder: null,
    });
    const result = await this.#transition(params.market, "cancel-order", context.record, next, [
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(params.market),
          meta(context.stored.auction),
          meta(context.auction.keyEpoch),
          meta(releaseKey),
          meta(counterAddress, true),
          meta(context.privateBalance, true),
          meta(context.stored.activeOrder, true),
          meta(context.stored.queueEvent),
          meta(event, true),
          meta(SYSTEM_PROGRAM_ADDRESS),
        ],
        encodeCancelActiveOrder({
          clientOrderNonce: nonce,
          releasedEncryptedBase: output.releasedEncryptedBase,
          releasedEncryptedQuote: output.releasedEncryptedQuote,
          proof: output.proof,
        }),
      ),
    ]);
    return { ...result, proof: output };
  }

  /** Reclaim the rent of this owner's finished queued-order events in one auction. */
  async closeOrderEvents(params: CloseOrderEventsParams): Promise<ConfirmedTransaction> {
    const [activeOrder] = await deriveActiveOrderPda(
      params.auction as KitAddress,
      this.owner as KitAddress,
      this.programId as KitAddress,
    );
    return this.#send([
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(params.market),
          meta(params.auction),
          meta(activeOrder, true),
          ...params.events.map((event) => meta(event, true)),
        ],
        encodeCloseOrderEvents(),
      ),
    ]);
  }

  #settlementRequest(context: ActiveContext) {
    const index = context.auction.clearingIndex;
    const { demand, supply } = clearingQuantities(context.auction);
    if (demand === undefined || supply === undefined) {
      throw new BeatExError("INVALID_ACCOUNT_DATA", "clearing index is outside the auction grid");
    }
    return {
      market: addressBytes(context.queueEvent.market),
      auction: addressBytes(context.stored.auction),
      order: addressBytes(context.stored.queueEvent),
      activeOrder: addressBytes(context.stored.activeOrder),
      owner: addressBytes(this.owner),
      keyEpoch: addressBytes(context.auction.keyEpoch),
      clientOrderNonce: context.queueEvent.clientOrderNonce,
      auctionSequence: context.queueEvent.sequence,
      allocationRuleVersion: context.auction.allocationRuleVersion,
      orderCircuitVersion: context.auction.orderCircuitVersion,
      claimCircuitVersion: context.auction.claimCircuitVersion,
      curve: context.stored.curve,
      baseReserve: context.stored.baseReserve,
      quoteReserve: context.stored.quoteReserve,
      orderSalt: context.stored.orderSalt,
      expectedOrderCommitment: context.stored.orderCommitment,
      clearingIndex: index,
      clearingPrice: context.auction.clearingPrice,
      demandAtClearing: demand,
      supplyAtClearing: supply,
      clearingVolume: context.auction.clearingVolume,
      balanceRevision: context.record.state.balanceRevision,
      committeeKey: asFixedBytes(context.keyEpoch.babyjubEncryptionPubkey, 32),
      oldBalance: context.record.state.balance,
    };
  }

  async settleToBalance(params: SettleToBalanceParams): Promise<SettlementResult> {
    const context = await this.#activeContext(params.market, params.auctionId);
    if (context.auction.state !== AuctionState.Settled) {
      throw new BeatExError("AUCTION_NOT_SETTLED", "auction is not settled");
    }
    if (
      context.queueEvent.state !== QueuedOrderState.Folded ||
      context.queueEvent.withdrawalState !== WithdrawalState.Pending
    ) {
      throw new BeatExError("INVALID_ARGUMENT", "order is not eligible for settlement");
    }
    const output = await this.#config.prover.proveSettlement(
      this.#settlementRequest(context),
      params.proofOptions,
    );
    const [rolloverKey] = await deriveVerificationKeyPda(
      params.market as KitAddress,
      QueueCircuitKind.RolloverSettlement,
      circuitVersion(QueueCircuitKind.RolloverSettlement),
      this.programId as KitAddress,
    );
    const expectedPublicInput = reverse32(output.proof.publicInput);
    const settlement = {
      terminalNullifier: output.terminalNullifier,
      creditedEncryptedBase: output.creditedEncryptedBase,
      creditedEncryptedQuote: output.creditedEncryptedQuote,
    };
    const bindingAccounts = [
      meta(this.owner, false, true),
      meta(params.market),
      meta(context.stored.auction),
      meta(context.auction.keyEpoch),
      meta(context.stored.queueEvent),
      meta(context.stored.activeOrder),
      meta(context.privateBalance),
      meta(rolloverKey),
      meta(INSTRUCTIONS_SYSVAR_ADDRESS),
    ];
    const finalAccounts = bindingAccounts.map((account, index) =>
      index === 0 || index === 4 || index === 5 || index === 6 ? { ...account, writable: true } : account,
    );
    const next = nextState(context.record.state, {
      balance: output.creditedBalance,
      encryptedBase: output.creditedEncryptedBase,
      encryptedQuote: output.creditedEncryptedQuote,
      activeOrder: null,
    });
    const result = await this.#transition(params.market, "settle-to-balance", context.record, next, [
      this.#instruction(
        [meta(params.market), meta(rolloverKey)],
        encodeVerifyRolloverProof({
          circuitKind: QueueCircuitKind.RolloverSettlement,
          expectedPublicInput,
          proof: output.proof,
        }),
      ),
      this.#instruction(
        bindingAccounts,
        encodeBindRolloverSettlement({ settlement, expectedPublicInput }),
      ),
      this.#instruction(finalAccounts, encodeSettleOrderToBalance()),
    ]);
    return {
      ...result,
      auction: context.stored.auction,
      basePayout: output.basePayout,
      quotePayout: output.quotePayout,
      proof: output,
    };
  }

  async refundToBalance(params: RefundToBalanceParams): Promise<SettlementResult> {
    const context = await this.#activeContext(params.market, params.auctionId);
    if (context.auction.state !== AuctionState.Cancelled) {
      throw new BeatExError("INVALID_ARGUMENT", "auction is not cancelled");
    }
    const output = await this.#config.prover.proveCancellation(
      this.#releaseRequest(context),
      params.proofOptions,
    );
    const [releaseKey] = await deriveVerificationKeyPda(
      params.market as KitAddress,
      QueueCircuitKind.OrderModification,
      circuitVersion(QueueCircuitKind.OrderModification),
      this.programId as KitAddress,
    );
    const next = nextState(context.record.state, {
      balance: output.releasedBalance,
      encryptedBase: output.releasedEncryptedBase,
      encryptedQuote: output.releasedEncryptedQuote,
      activeOrder: null,
    });
    const result = await this.#transition(params.market, "refund-to-balance", context.record, next, [
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(params.market),
          meta(context.stored.auction),
          meta(context.auction.keyEpoch),
          meta(releaseKey),
          meta(context.privateBalance, true),
          meta(context.stored.activeOrder, true),
          meta(context.stored.queueEvent, true),
        ],
        encodeRefundOrderToBalance({
          releasedEncryptedBase: output.releasedEncryptedBase,
          releasedEncryptedQuote: output.releasedEncryptedQuote,
          proof: output.proof,
        }),
      ),
    ]);
    return { ...result, auction: context.stored.auction, proof: output };
  }

  async rollover(params: RolloverParams): Promise<RolloverResult> {
    validateCurve(params.nextCurve);
    const context = await this.#activeContext(params.market, params.previousAuctionId);
    if (context.auction.state !== AuctionState.Settled) {
      throw new BeatExError("AUCTION_NOT_SETTLED", "previous auction is not settled");
    }
    if (
      context.queueEvent.state !== QueuedOrderState.Folded ||
      context.queueEvent.withdrawalState !== WithdrawalState.Pending
    ) {
      throw new BeatExError("INVALID_ARGUMENT", "previous order is not rollover eligible");
    }
    const nextAuction = await this.#currentAuction(
      params.market,
      params.initializeAuction ?? true,
    );
    if (nextAuction.auctionId === params.previousAuctionId) {
      throw new BeatExError("INVALID_ARGUMENT", "the next auction has not started yet");
    }
    if (nextAuction.auction.keyEpoch !== context.auction.keyEpoch) {
      throw new BeatExError(
        "INVALID_ARGUMENT",
        "rollover requires the previous and next auctions to use the same key epoch",
      );
    }
    const [nextCounterAddress] = await deriveSubmissionCounterPda(
      nextAuction.auctionAddress as KitAddress,
      this.programId as KitAddress,
    );
    const nonce = params.clientOrderNonce ?? randomU64();
    const output = await this.#config.prover.proveRollover(
      {
        settlement: this.#settlementRequest(context),
        nextOrder: {
          auction: addressBytes(nextAuction.auctionAddress),
          owner: addressBytes(this.owner),
          clientOrderNonce: nonce,
          referencePrice: nextAuction.auction.referencePrice,
          gridStepBps: nextAuction.auction.gridStepBps,
          committeeKey: asFixedBytes(context.keyEpoch.babyjubEncryptionPubkey, 32),
          curve: params.nextCurve,
        },
      },
      params.proofOptions,
    );
    const [[nextOrder], [nextActiveOrder], [rolloverKey], [orderKey]] = await Promise.all([
      deriveRolloverOrderPda(nextAuction.auctionAddress as KitAddress, this.owner as KitAddress, this.programId as KitAddress),
      deriveActiveOrderPda(nextAuction.auctionAddress as KitAddress, this.owner as KitAddress, this.programId as KitAddress),
      deriveVerificationKeyPda(params.market as KitAddress, QueueCircuitKind.RolloverSettlement, circuitVersion(QueueCircuitKind.RolloverSettlement), this.programId as KitAddress),
      deriveVerificationKeyPda(params.market as KitAddress, QueueCircuitKind.OrderSubmission, nextAuction.auction.orderCircuitVersion, this.programId as KitAddress),
    ]);
    const settlementExpectedInput = reverse32(output.settlement.proof.publicInput);
    const orderExpectedInput = reverse32(output.nextOrder.proof.publicInput);
    const settlement = {
      terminalNullifier: output.settlement.terminalNullifier,
      creditedEncryptedBase: output.settlement.creditedEncryptedBase,
      creditedEncryptedQuote: output.settlement.creditedEncryptedQuote,
    };
    const nextOrderArgs = {
      clientOrderNonce: nonce,
      orderCommitment: output.nextOrder.orderCommitment,
      ciphertexts: concatCiphertexts(output.nextOrder.ciphertexts),
      newEncryptedBase: output.nextOrder.newEncryptedBase,
      newEncryptedQuote: output.nextOrder.newEncryptedQuote,
    };
    const active: StoredActiveOrder = {
      auctionId: nextAuction.auctionId,
      auction: nextAuction.auctionAddress,
      activeOrder: nextActiveOrder,
      queueEvent: nextOrder,
      clientOrderNonce: nonce,
      curve: params.nextCurve,
      orderSalt: output.nextOrder.orderSalt,
      orderCommitment: output.nextOrder.orderCommitment,
      baseReserve: output.nextOrder.baseReserve,
      quoteReserve: output.nextOrder.quoteReserve,
    };
    const next = nextState(context.record.state, {
      balance: output.nextOrder.newBalance,
      encryptedBase: output.nextOrder.newEncryptedBase,
      encryptedQuote: output.nextOrder.newEncryptedQuote,
      activeOrder: active,
    });
    const result = await this.#transition(params.market, "rollover", context.record, next, [
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(params.market),
          meta(nextAuction.auctionAddress),
          meta(nextCounterAddress),
          meta(nextOrder, true),
          meta(nextActiveOrder, true),
          meta(INSTRUCTIONS_SYSVAR_ADDRESS),
          meta(SYSTEM_PROGRAM_ADDRESS),
        ],
        encodePrepareRollover(),
      ),
      this.#instruction(
        [meta(params.market), meta(rolloverKey)],
        encodeVerifyRolloverProof({
          circuitKind: QueueCircuitKind.RolloverSettlement,
          expectedPublicInput: settlementExpectedInput,
          proof: output.settlement.proof,
        }),
      ),
      this.#instruction(
        [
          meta(this.owner, false, true),
          meta(params.market),
          meta(context.stored.auction),
          meta(context.auction.keyEpoch),
          meta(context.stored.queueEvent),
          meta(context.stored.activeOrder),
          meta(context.privateBalance),
          meta(rolloverKey),
          meta(INSTRUCTIONS_SYSVAR_ADDRESS),
        ],
        encodeBindRolloverSettlement({ settlement, expectedPublicInput: settlementExpectedInput }),
      ),
      this.#instruction(
        [meta(params.market), meta(orderKey)],
        encodeVerifyRolloverProof({
          circuitKind: QueueCircuitKind.OrderSubmission,
          expectedPublicInput: orderExpectedInput,
          proof: output.nextOrder.proof,
        }),
      ),
      this.#instruction(
        [
          meta(this.owner, false, true),
          meta(params.market),
          meta(nextAuction.auctionAddress),
          meta(context.auction.keyEpoch),
          meta(orderKey),
          meta(INSTRUCTIONS_SYSVAR_ADDRESS),
        ],
        encodeBindRolloverOrder({
          nextOrder: nextOrderArgs,
          creditedEncryptedBase: output.settlement.creditedEncryptedBase,
          creditedEncryptedQuote: output.settlement.creditedEncryptedQuote,
          expectedPublicInput: orderExpectedInput,
        }),
      ),
      this.#instruction(
        [
          meta(this.owner, true, true),
          meta(params.market),
          meta(context.stored.auction),
          meta(nextAuction.auctionAddress),
          meta(context.auction.keyEpoch),
          meta(context.stored.queueEvent, true),
          meta(context.stored.activeOrder, true),
          meta(nextCounterAddress, true),
          meta(context.privateBalance, true),
          meta(rolloverKey),
          meta(orderKey),
          meta(nextOrder, true),
          meta(nextActiveOrder, true),
          meta(INSTRUCTIONS_SYSVAR_ADDRESS),
        ],
        encodeRolloverOrder(),
      ),
    ]);
    return {
      ...result,
      previousAuction: context.stored.auction,
      auction: nextAuction.auctionAddress,
      auctionId: nextAuction.auctionId,
      activeOrder: nextActiveOrder,
      queueEvent: nextOrder,
      basePayout: output.settlement.basePayout,
      quotePayout: output.settlement.quotePayout,
      ...(nextAuction.initializationTransaction === undefined
        ? {}
        : { auctionInitializationTransaction: nextAuction.initializationTransaction }),
      proof: output,
    };
  }

  async withdraw(params: WithdrawParams): Promise<WithdrawResult> {
    asU64(params.baseAmount, "baseAmount");
    asU64(params.quoteAmount, "quoteAmount");
    if (params.baseAmount === 0n && params.quoteAmount === 0n) {
      throw new BeatExError("INVALID_ARGUMENT", "withdrawal amount cannot be zero for both assets");
    }
    const record = await this.getPrivateState(params.market);
    if (record === null) throw new BeatExError("PRIVATE_STATE_MISSING", "private balance is not initialized");
    const marketState = await this.getMarket(params.market);
    if (record.state.keyEpoch !== marketState.currentKeyEpoch) {
      throw new BeatExError("PRIVATE_STATE_CONFLICT", "private balance belongs to a different key epoch");
    }
    const keyEpoch = await this.#required<KeyEpochAccount>(
      marketState.currentKeyEpoch,
      decodeKeyEpoch,
      "key epoch",
    );
    const output = await this.#config.prover.proveWithdrawal(
      {
        market: addressBytes(params.market),
        owner: addressBytes(this.owner),
        keyEpoch: addressBytes(marketState.currentKeyEpoch),
        committeeKey: asFixedBytes(keyEpoch.babyjubEncryptionPubkey, 32),
        currentRevision: record.state.balanceRevision,
        baseAmount: params.baseAmount,
        quoteAmount: params.quoteAmount,
        currentBalance: record.state.balance,
      },
      params.proofOptions,
    );
    const [[privateBalance], [depositKey], [vaultAuthority]] = await Promise.all([
      derivePrivateBalancePda(params.market as KitAddress, this.owner as KitAddress, this.programId as KitAddress),
      deriveVerificationKeyPda(params.market as KitAddress, QueueCircuitKind.BalanceDeposit, circuitVersion(QueueCircuitKind.BalanceDeposit), this.programId as KitAddress),
      deriveVaultAuthorityPda(params.market as KitAddress, this.programId as KitAddress),
    ]);
    const next = nextState(record.state, {
      balance: output.remainingBalance,
      encryptedBase: output.remainingEncryptedBase,
      encryptedQuote: output.remainingEncryptedQuote,
      activeOrder: record.state.activeOrder,
    });
    const result = await this.#transition(
      params.market,
      "withdraw",
      record,
      next,
      [
        this.#instruction(
          [
            meta(this.owner, true, true),
            meta(params.market),
            meta(marketState.currentKeyEpoch),
            meta(depositKey),
            meta(privateBalance, true),
            meta(marketState.baseMint),
            meta(marketState.quoteMint),
            meta(marketState.baseVault, true),
            meta(marketState.quoteVault, true),
            meta(params.baseDestination, true),
            meta(params.quoteDestination, true),
            meta(vaultAuthority),
            meta(TOKEN_2022_PROGRAM_ADDRESS),
          ],
          encodeWithdrawPublicBalance({
            baseAmount: params.baseAmount,
            quoteAmount: params.quoteAmount,
            remainingEncryptedBase: output.remainingEncryptedBase,
            remainingEncryptedQuote: output.remainingEncryptedQuote,
            proof: output.proof,
          }),
        ),
      ],
      "legacy",
    );
    return {
      ...result,
      baseAmount: params.baseAmount,
      quoteAmount: params.quoteAmount,
      proof: output,
    };
  }

  /**
   * Move `amount` of one token from one market's private balance to another's
   * in a single transaction: a public withdrawal from `from` into the owner's
   * token account, then a public deposit from it into `to`. Opens `to`'s
   * private balance first (a separate transaction) when it has none. Both
   * markets' private states commit or roll back together.
   */
  async moveBetweenMarkets(params: MoveBetweenMarketsParams): Promise<MoveBetweenMarketsResult> {
    asU64(params.amount, "amount");
    if (params.amount === 0n) {
      throw new BeatExError("INVALID_ARGUMENT", "move amount cannot be zero");
    }
    if (params.from === params.to) {
      throw new BeatExError("INVALID_ARGUMENT", "cannot move funds within one market");
    }
    const [fromMarket, toMarket] = await Promise.all([
      this.getMarket(params.from),
      this.getMarket(params.to),
    ]);
    const side = (market: MarketAccount, label: string): "base" | "quote" => {
      if (market.baseMint === params.mint) return "base";
      if (market.quoteMint === params.mint) return "quote";
      throw new BeatExError("INVALID_ARGUMENT", `${label} market does not trade this token`);
    };
    const fromSide = side(fromMarket, "source");
    const toSide = side(toMarket, "destination");

    const fromRecord = await this.getPrivateState(params.from);
    if (fromRecord === null) {
      throw new BeatExError("PRIVATE_STATE_MISSING", "source market has no private balance");
    }
    if (fromRecord.state.keyEpoch !== fromMarket.currentKeyEpoch) {
      throw new BeatExError("PRIVATE_STATE_CONFLICT", "source balance belongs to a different key epoch");
    }
    const available =
      fromSide === "base" ? fromRecord.state.balance.baseValue : fromRecord.state.balance.quoteValue;
    if (params.amount > available) {
      throw new BeatExError("INVALID_ARGUMENT", "move amount exceeds the source market's free balance");
    }
    const initialized = await this.#ensurePrivateBalance(params.to, toMarket, "legacy");
    const toRecord = initialized.record;

    const [fromEpoch, toEpoch] = await Promise.all([
      this.#required<KeyEpochAccount>(fromMarket.currentKeyEpoch, decodeKeyEpoch, "key epoch"),
      this.#required<KeyEpochAccount>(toMarket.currentKeyEpoch, decodeKeyEpoch, "key epoch"),
    ]);
    const amounts = (target: "base" | "quote") => ({
      baseAmount: target === "base" ? params.amount : 0n,
      quoteAmount: target === "quote" ? params.amount : 0n,
    });
    const out = amounts(fromSide);
    const into = amounts(toSide);

    // Proofs run one after the other on the single prover worker.
    const withdrawal = await this.#config.prover.proveWithdrawal(
      {
        market: addressBytes(params.from),
        owner: addressBytes(this.owner),
        keyEpoch: addressBytes(fromMarket.currentKeyEpoch),
        committeeKey: asFixedBytes(fromEpoch.babyjubEncryptionPubkey, 32),
        currentRevision: fromRecord.state.balanceRevision,
        ...out,
        currentBalance: fromRecord.state.balance,
      },
      params.proofOptions,
    );
    const deposit = await this.#config.prover.proveDeposit(
      {
        market: addressBytes(params.to),
        owner: addressBytes(this.owner),
        keyEpoch: addressBytes(toMarket.currentKeyEpoch),
        committeeKey: asFixedBytes(toEpoch.babyjubEncryptionPubkey, 32),
        oldRevision: toRecord.state.balanceRevision,
        ...into,
        oldBalance: toRecord.state.balance,
      },
      params.proofOptions,
    );

    const owner = this.owner as KitAddress;
    const program = this.programId as KitAddress;
    const tokenAccount = async (mint: string) =>
      (await deriveAssociatedToken2022Address(owner, mint as KitAddress, TOKEN_2022_PROGRAM_ADDRESS))[0];
    const [
      [fromBalance],
      [toBalance],
      [fromDepositKey],
      [toDepositKey],
      [fromVaultAuthority],
      [toVaultAuthority],
      fromBaseAccount,
      fromQuoteAccount,
      toBaseAccount,
      toQuoteAccount,
    ] = await Promise.all([
      derivePrivateBalancePda(params.from as KitAddress, owner, program),
      derivePrivateBalancePda(params.to as KitAddress, owner, program),
      deriveVerificationKeyPda(params.from as KitAddress, QueueCircuitKind.BalanceDeposit, circuitVersion(QueueCircuitKind.BalanceDeposit), program),
      deriveVerificationKeyPda(params.to as KitAddress, QueueCircuitKind.BalanceDeposit, circuitVersion(QueueCircuitKind.BalanceDeposit), program),
      deriveVaultAuthorityPda(params.from as KitAddress, program),
      deriveVaultAuthorityPda(params.to as KitAddress, program),
      tokenAccount(fromMarket.baseMint),
      tokenAccount(fromMarket.quoteMint),
      tokenAccount(toMarket.baseMint),
      tokenAccount(toMarket.quoteMint),
    ]);

    // The program validates every token account, even for a zero amount, so
    // make sure the owner has one for each token either market touches.
    const tokenAccounts = new Map<string, string>([
      [fromBaseAccount, fromMarket.baseMint],
      [fromQuoteAccount, fromMarket.quoteMint],
      [toBaseAccount, toMarket.baseMint],
      [toQuoteAccount, toMarket.quoteMint],
    ]);
    const createAccounts = [...tokenAccounts].map(([account, mint]) => ({
      programId: ASSOCIATED_TOKEN_PROGRAM_ADDRESS,
      accounts: [
        meta(this.owner, true, true),
        meta(account, true),
        meta(this.owner),
        meta(mint),
        meta(SYSTEM_PROGRAM_ADDRESS),
        meta(TOKEN_2022_PROGRAM_ADDRESS),
      ],
      // CreateIdempotent: a no-op when the account already exists.
      data: Uint8Array.of(1),
    }));

    const fromNext = nextState(fromRecord.state, {
      balance: withdrawal.remainingBalance,
      encryptedBase: withdrawal.remainingEncryptedBase,
      encryptedQuote: withdrawal.remainingEncryptedQuote,
      activeOrder: fromRecord.state.activeOrder,
    });
    const toNext = nextState(toRecord.state, {
      balance: deposit.newBalance,
      encryptedBase: deposit.newEncryptedBase,
      encryptedQuote: deposit.newEncryptedQuote,
      activeOrder: toRecord.state.activeOrder,
    });

    const result = await this.#transitions(
      [
        { market: params.from, operation: "withdraw", currentRecord: fromRecord, next: fromNext },
        { market: params.to, operation: "deposit", currentRecord: toRecord, next: toNext },
      ],
      [
        ...createAccounts,
        this.#instruction(
          [
            meta(this.owner, true, true),
            meta(params.from),
            meta(fromMarket.currentKeyEpoch),
            meta(fromDepositKey),
            meta(fromBalance, true),
            meta(fromMarket.baseMint),
            meta(fromMarket.quoteMint),
            meta(fromMarket.baseVault, true),
            meta(fromMarket.quoteVault, true),
            meta(fromBaseAccount, true),
            meta(fromQuoteAccount, true),
            meta(fromVaultAuthority),
            meta(TOKEN_2022_PROGRAM_ADDRESS),
          ],
          encodeWithdrawPublicBalance({
            ...out,
            remainingEncryptedBase: withdrawal.remainingEncryptedBase,
            remainingEncryptedQuote: withdrawal.remainingEncryptedQuote,
            proof: withdrawal.proof,
          }),
        ),
        this.#instruction(
          [
            meta(this.owner, true, true),
            meta(params.to),
            meta(toMarket.currentKeyEpoch),
            meta(toDepositKey),
            meta(toBalance, true),
            meta(toMarket.baseMint),
            meta(toMarket.quoteMint),
            meta(toBaseAccount, true),
            meta(toQuoteAccount, true),
            meta(toMarket.baseVault, true),
            meta(toMarket.quoteVault, true),
            meta(toVaultAuthority),
            meta(TOKEN_2022_PROGRAM_ADDRESS),
          ],
          encodeDepositCollateral({
            ...into,
            newEncryptedBase: deposit.newEncryptedBase,
            newEncryptedQuote: deposit.newEncryptedQuote,
            proof: deposit.proof,
          }),
        ),
      ],
      // Two proofs and two markets' accounts exceed the legacy size limit.
      1,
    );
    return {
      transaction: result.transaction,
      fromPrivateState: result.privateStates[0]!,
      toPrivateState: result.privateStates[1]!,
      ...(initialized.initializationTransaction === undefined
        ? {}
        : { initializationTransaction: initialized.initializationTransaction }),
    };
  }

  /** Complete a confirmation that survived a browser reload after broadcast. */
  /**
   * Resolve an interrupted transition: commit it if its transaction confirmed,
   * drop it if the transaction failed or expired. Returns the resulting record,
   * or null when nothing was ever committed (a first deposit that never landed).
   */
  async recoverPendingTransition(market: string): Promise<PrivateStateRecord | null> {
    const key = this.#stateKey(market);
    const snapshot = await this.#config.storage.load(key);
    const pending = snapshot.pending;
    if (pending === null) return snapshot.record;
    if (
      pending.transactionSignature === undefined ||
      pending.blockhash === undefined ||
      pending.lastValidBlockHeight === undefined
    ) {
      // Staged without a signature (older SDK versions recorded it only after
      // sending). The chain is the only witness of whether it applied.
      if (await this.#onchainMatches(market, pending.nextState)) {
        return this.#config.storage.commitConfirmed(key, pending.id, {
          signature: "reconciled-from-chain",
          slot: 0n,
          confirmationStatus: "confirmed",
        });
      }
      if (Date.now() - pending.createdAtMs < UNSIGNED_TRANSITION_EXPIRY_MS) {
        throw new BeatExError(
          "PRIVATE_STATE_TRANSITION_PENDING",
          "the interrupted transaction may still land; try again in a few minutes",
          { retryable: true, context: { transitionId: pending.id } },
        );
      }
      await this.#config.storage.abort(key, pending.id);
      return snapshot.record;
    }
    try {
      const confirmation = await this.#config.rpc.confirmTransaction(pending.transactionSignature, {
        commitment: "confirmed",
        blockhash: pending.blockhash,
        lastValidBlockHeight: pending.lastValidBlockHeight,
      });
      return await this.#config.storage.commitConfirmed(key, pending.id, confirmation);
    } catch (error) {
      if (!(error instanceof TransactionOutcomeError)) throw error;
      await this.#config.storage.abort(key, pending.id);
      return snapshot.record;
    }
  }

  /** Abort only a transition known never to have been broadcast. */
  async discardUnbroadcastTransition(market: string): Promise<void> {
    const key = this.#stateKey(market);
    const snapshot = await this.#config.storage.load(key);
    const pending = snapshot.pending;
    if (pending === null) return;
    if (pending.transactionSignature !== undefined) {
      throw new BeatExError(
        "PRIVATE_STATE_TRANSITION_PENDING",
        "a transaction signature was recorded; recover or reconcile it instead of discarding",
        { context: { transitionId: pending.id, signature: pending.transactionSignature } },
      );
    }
    await this.#config.storage.abort(key, pending.id);
  }
}

export type CreateBeatExClientConfig = Omit<BeatExClientConfig, "transactionCompiler"> & {
  readonly transactionCompiler?: BeatExClientConfig["transactionCompiler"];
};

export function createBeatExClient(config: CreateBeatExClientConfig): BeatExClient {
  return new BeatExClient({
    ...config,
    transactionCompiler: config.transactionCompiler ?? createKitTransactionCompiler(),
  });
}
