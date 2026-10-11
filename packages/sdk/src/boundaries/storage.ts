import { BeatExError, PrivateStateConflictError } from "./errors.js";
import type {
  Address,
  Bytes32,
  Ciphertext,
  Cluster,
  ConfirmedTransaction,
  OrderCurve,
  PrivateBalanceOpening,
} from "./primitives.js";

export interface PrivateStateKey {
  readonly cluster: Cluster;
  readonly programId: Address;
  readonly market: Address;
  readonly owner: Address;
}

export interface StoredActiveOrder {
  readonly auctionId: bigint;
  readonly auction: Address;
  readonly activeOrder: Address;
  readonly queueEvent: Address;
  readonly clientOrderNonce: bigint;
  readonly curve: OrderCurve;
  readonly orderSalt: Bytes32;
  readonly orderCommitment: Bytes32;
  readonly baseReserve: bigint;
  readonly quoteReserve: bigint;
}

export interface TraderPrivateState {
  /** Revision of the on-chain EncryptedPrivateBalance account. */
  readonly balanceRevision: bigint;
  readonly keyEpoch: Address;
  readonly balance: PrivateBalanceOpening;
  readonly encryptedBase: Ciphertext;
  readonly encryptedQuote: Ciphertext;
  readonly activeOrder: StoredActiveOrder | null;
}

export interface PrivateStateRecord {
  /** Local optimistic-concurrency revision; independent of balanceRevision. */
  readonly storageRevision: bigint;
  readonly state: TraderPrivateState;
  readonly updatedAtMs: number;
  readonly lastConfirmedTransaction?: ConfirmedTransaction;
}

export type PrivateStateOperation =
  | "initialize"
  | "deposit"
  | "place-order"
  | "replace-order"
  | "cancel-order"
  | "settle-to-balance"
  | "refund-to-balance"
  | "rollover"
  | "withdraw";

export interface StagePrivateStateTransition {
  readonly id: string;
  readonly operation: PrivateStateOperation;
  /** null means that no state record may exist yet. */
  readonly expectedStorageRevision: bigint | null;
  readonly nextState: TraderPrivateState;
  readonly createdAtMs?: number;
}

export interface StagedPrivateStateTransition extends StagePrivateStateTransition {
  readonly createdAtMs: number;
  readonly transactionSignature?: string;
  readonly blockhash?: string;
  readonly lastValidBlockHeight?: bigint;
}

export interface PrivateStateSnapshot {
  readonly record: PrivateStateRecord | null;
  readonly pending: StagedPrivateStateTransition | null;
}

/**
 * Private state is a two-phase journal. `stage` persists the next opening before
 * broadcast; `commitConfirmed` atomically advances it only after chain confirmation.
 */
export interface PrivateStateStorage {
  load(key: PrivateStateKey): Promise<PrivateStateSnapshot>;
  stage(
    key: PrivateStateKey,
    transition: StagePrivateStateTransition,
  ): Promise<StagedPrivateStateTransition>;
  markBroadcast(
    key: PrivateStateKey,
    transitionId: string,
    transactionSignature: string,
    blockhash: string,
    lastValidBlockHeight: bigint,
  ): Promise<StagedPrivateStateTransition>;
  commitConfirmed(
    key: PrivateStateKey,
    transitionId: string,
    confirmation: ConfirmedTransaction,
  ): Promise<PrivateStateRecord>;
  abort(key: PrivateStateKey, transitionId: string): Promise<void>;
}

interface StoredEnvelope {
  record: PrivateStateRecord | null;
  pending: StagedPrivateStateTransition | null;
}

function emptyEnvelope(): StoredEnvelope {
  return { record: null, pending: null };
}

function assertCanStage(
  envelope: StoredEnvelope,
  transition: StagePrivateStateTransition,
): StagedPrivateStateTransition {
  if (envelope.pending !== null) {
    throw new BeatExError(
      "PRIVATE_STATE_TRANSITION_PENDING",
      `private-state transition ${envelope.pending.id} is already pending`,
      { retryable: true, context: { pendingTransitionId: envelope.pending.id } },
    );
  }
  const actualRevision = envelope.record?.storageRevision ?? null;
  if (actualRevision !== transition.expectedStorageRevision) {
    throw new PrivateStateConflictError("private-state revision changed before staging", {
      expectedRevision: transition.expectedStorageRevision?.toString() ?? null,
      actualRevision: actualRevision?.toString() ?? null,
    });
  }
  if (transition.id.length === 0) {
    throw new BeatExError("INVALID_ARGUMENT", "transition id must not be empty");
  }
  return {
    ...transition,
    createdAtMs: transition.createdAtMs ?? Date.now(),
  };
}

function commitEnvelope(
  envelope: StoredEnvelope,
  transitionId: string,
  confirmation: ConfirmedTransaction,
): PrivateStateRecord {
  const pending = envelope.pending;
  if (pending === null || pending.id !== transitionId) {
    throw new PrivateStateConflictError("pending private-state transition was not found", {
      transitionId,
      actualTransitionId: pending?.id ?? null,
    });
  }
  const actualRevision = envelope.record?.storageRevision ?? null;
  if (actualRevision !== pending.expectedStorageRevision) {
    throw new PrivateStateConflictError("private-state revision changed before commit", {
      expectedRevision: pending.expectedStorageRevision?.toString() ?? null,
      actualRevision: actualRevision?.toString() ?? null,
    });
  }
  return {
    storageRevision: (actualRevision ?? -1n) + 1n,
    state: pending.nextState,
    updatedAtMs: Date.now(),
    lastConfirmedTransaction: confirmation,
  };
}

function markBroadcastEnvelope(
  envelope: StoredEnvelope,
  transitionId: string,
  transactionSignature: string,
  blockhash: string,
  lastValidBlockHeight: bigint,
): StagedPrivateStateTransition {
  const pending = envelope.pending;
  if (pending === null || pending.id !== transitionId) {
    throw new PrivateStateConflictError("pending private-state transition was not found", {
      transitionId,
      actualTransitionId: pending?.id ?? null,
    });
  }
  if (pending.transactionSignature !== undefined && pending.transactionSignature !== transactionSignature) {
    throw new PrivateStateConflictError("transition is already bound to another transaction", {
      transitionId,
      actualTransactionSignature: pending.transactionSignature,
    });
  }
  return { ...pending, transactionSignature, blockhash, lastValidBlockHeight };
}

function encodeStorageKey(key: PrivateStateKey): string {
  const components = [key.cluster, key.programId, key.market, key.owner];
  return components.map((value) => `${value.length}:${value}`).join("|");
}

function cloneValue<T>(value: T): T {
  return decodeJson<T>(encodeJson(value));
}

export class InMemoryPrivateStateStorage implements PrivateStateStorage {
  readonly #entries = new Map<string, StoredEnvelope>();

  async load(key: PrivateStateKey): Promise<PrivateStateSnapshot> {
    return cloneValue(this.#entries.get(encodeStorageKey(key)) ?? emptyEnvelope());
  }

  async stage(
    key: PrivateStateKey,
    transition: StagePrivateStateTransition,
  ): Promise<StagedPrivateStateTransition> {
    const encodedKey = encodeStorageKey(key);
    const envelope = this.#entries.get(encodedKey) ?? emptyEnvelope();
    const pending = assertCanStage(envelope, transition);
    this.#entries.set(encodedKey, cloneValue({ ...envelope, pending }));
    return cloneValue(pending);
  }

  async commitConfirmed(
    key: PrivateStateKey,
    transitionId: string,
    confirmation: ConfirmedTransaction,
  ): Promise<PrivateStateRecord> {
    const encodedKey = encodeStorageKey(key);
    const envelope = this.#entries.get(encodedKey) ?? emptyEnvelope();
    const record = commitEnvelope(envelope, transitionId, confirmation);
    this.#entries.set(encodedKey, cloneValue({ record, pending: null }));
    return cloneValue(record);
  }

  async markBroadcast(
    key: PrivateStateKey,
    transitionId: string,
    transactionSignature: string,
    blockhash: string,
    lastValidBlockHeight: bigint,
  ): Promise<StagedPrivateStateTransition> {
    const encodedKey = encodeStorageKey(key);
    const envelope = this.#entries.get(encodedKey) ?? emptyEnvelope();
    const pending = markBroadcastEnvelope(
      envelope,
      transitionId,
      transactionSignature,
      blockhash,
      lastValidBlockHeight,
    );
    this.#entries.set(encodedKey, cloneValue({ ...envelope, pending }));
    return cloneValue(pending);
  }

  async abort(key: PrivateStateKey, transitionId: string): Promise<void> {
    const encodedKey = encodeStorageKey(key);
    const envelope = this.#entries.get(encodedKey) ?? emptyEnvelope();
    if (envelope.pending === null) return;
    if (envelope.pending.id !== transitionId) {
      throw new PrivateStateConflictError("refusing to abort a different pending transition", {
        transitionId,
        actualTransitionId: envelope.pending.id,
      });
    }
    this.#entries.set(encodedKey, cloneValue({ ...envelope, pending: null }));
  }
}

/**
 * localStorage-shaped key/value persistence. Methods may be asynchronous, which lets an
 * application layer encryption or IndexedDB underneath the same journal semantics.
 */
export interface WebStorageLike {
  getItem(key: string): string | null | Promise<string | null>;
  setItem(key: string, value: string): void | Promise<void>;
}

export interface BrowserLockManagerLike {
  request<T>(name: string, callback: () => Promise<T>): Promise<T>;
}

export interface BrowserPrivateStateStorageOptions {
  readonly prefix?: string;
  /** Usually `navigator.locks`; required for atomic mutation across tabs/workers. */
  readonly lockManager?: BrowserLockManagerLike;
  /** Allows a process-local mutex when Web Locks are unavailable. Not cross-tab safe. */
  readonly allowSingleContextFallback?: boolean;
}

/** localStorage-compatible (sync or async) persistence with a Web Locks atomicity boundary. */
export class BrowserPrivateStateStorage implements PrivateStateStorage {
  readonly #storage: WebStorageLike;
  readonly #prefix: string;
  readonly #locks: BrowserLockManagerLike | undefined;
  readonly #allowFallback: boolean;
  readonly #queues = new Map<string, Promise<void>>();

  constructor(storage: WebStorageLike, options: BrowserPrivateStateStorageOptions = {}) {
    this.#storage = storage;
    this.#prefix = options.prefix ?? "beatex:private-state:v1:";
    this.#locks = options.lockManager ?? browserLockManager();
    this.#allowFallback = options.allowSingleContextFallback ?? false;
  }

  async load(key: PrivateStateKey): Promise<PrivateStateSnapshot> {
    return cloneValue(await this.#read(this.#entryKey(key)));
  }

  async stage(
    key: PrivateStateKey,
    transition: StagePrivateStateTransition,
  ): Promise<StagedPrivateStateTransition> {
    return this.#exclusive(key, async () => {
      const storageKey = this.#entryKey(key);
      const envelope = await this.#read(storageKey);
      const pending = assertCanStage(envelope, transition);
      await this.#write(storageKey, { ...envelope, pending });
      return cloneValue(pending);
    });
  }

  async commitConfirmed(
    key: PrivateStateKey,
    transitionId: string,
    confirmation: ConfirmedTransaction,
  ): Promise<PrivateStateRecord> {
    return this.#exclusive(key, async () => {
      const storageKey = this.#entryKey(key);
      const envelope = await this.#read(storageKey);
      const record = commitEnvelope(envelope, transitionId, confirmation);
      await this.#write(storageKey, { record, pending: null });
      return cloneValue(record);
    });
  }

  async markBroadcast(
    key: PrivateStateKey,
    transitionId: string,
    transactionSignature: string,
    blockhash: string,
    lastValidBlockHeight: bigint,
  ): Promise<StagedPrivateStateTransition> {
    return this.#exclusive(key, async () => {
      const storageKey = this.#entryKey(key);
      const envelope = await this.#read(storageKey);
      const pending = markBroadcastEnvelope(
        envelope,
        transitionId,
        transactionSignature,
        blockhash,
        lastValidBlockHeight,
      );
      await this.#write(storageKey, { ...envelope, pending });
      return cloneValue(pending);
    });
  }

  async abort(key: PrivateStateKey, transitionId: string): Promise<void> {
    await this.#exclusive(key, async () => {
      const storageKey = this.#entryKey(key);
      const envelope = await this.#read(storageKey);
      if (envelope.pending === null) return;
      if (envelope.pending.id !== transitionId) {
        throw new PrivateStateConflictError("refusing to abort a different pending transition", {
          transitionId,
          actualTransitionId: envelope.pending.id,
        });
      }
      await this.#write(storageKey, { ...envelope, pending: null });
    });
  }

  #entryKey(key: PrivateStateKey): string {
    return this.#prefix + encodeStorageKey(key);
  }

  async #read(key: string): Promise<StoredEnvelope> {
    const value = await this.#storage.getItem(key);
    return value === null ? emptyEnvelope() : decodeJson<StoredEnvelope>(value);
  }

  async #write(key: string, envelope: StoredEnvelope): Promise<void> {
    await this.#storage.setItem(key, encodeJson(envelope));
  }

  async #exclusive<T>(key: PrivateStateKey, callback: () => Promise<T>): Promise<T> {
    const lockName = this.#prefix + "lock:" + encodeStorageKey(key);
    if (this.#locks !== undefined) return this.#locks.request(lockName, callback);
    if (!this.#allowFallback) {
      throw new BeatExError(
        "PRIVATE_STATE_ATOMICITY_UNAVAILABLE",
        "Web Locks are unavailable; refusing a potentially non-atomic private-state update",
      );
    }

    const previous = this.#queues.get(lockName) ?? Promise.resolve();
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const queued = previous.then(() => gate);
    this.#queues.set(lockName, queued);
    await previous;
    try {
      return await callback();
    } finally {
      release();
      if (this.#queues.get(lockName) === queued) this.#queues.delete(lockName);
    }
  }
}

function browserLockManager(): BrowserLockManagerLike | undefined {
  const candidate = globalThis as unknown as {
    navigator?: { locks?: BrowserLockManagerLike };
  };
  return candidate.navigator?.locks;
}

function encodeJson(value: unknown): string {
  return JSON.stringify(value, (_key, item: unknown) => {
    if (typeof item === "bigint") return { $beatexType: "bigint", value: item.toString() };
    if (item instanceof Uint8Array) {
      let hex = "";
      for (const byte of item) hex += byte.toString(16).padStart(2, "0");
      return { $beatexType: "bytes", value: hex };
    }
    return item;
  });
}

function decodeJson<T>(value: string): T {
  return JSON.parse(value, (_key, item: unknown) => {
    if (typeof item !== "object" || item === null || !("$beatexType" in item)) return item;
    const tagged = item as { $beatexType: unknown; value?: unknown };
    if (tagged.$beatexType === "bigint" && typeof tagged.value === "string") {
      return BigInt(tagged.value);
    }
    if (tagged.$beatexType === "bytes" && typeof tagged.value === "string") {
      if (tagged.value.length % 2 !== 0 || !/^[0-9a-f]*$/.test(tagged.value)) {
        throw new BeatExError("INVALID_ACCOUNT_DATA", "stored private state contains invalid bytes");
      }
      const bytes = new Uint8Array(tagged.value.length / 2);
      for (let index = 0; index < bytes.length; index += 1) {
        bytes[index] = Number.parseInt(tagged.value.slice(index * 2, index * 2 + 2), 16);
      }
      return bytes;
    }
    return item;
  }) as T;
}
