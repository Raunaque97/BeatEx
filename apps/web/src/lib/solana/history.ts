import { publicEnv } from "@/lib/config/public-env";

/** BeatEx program instructions a trader's own transactions can contain. */
const BEATEX_ACTIONS: Readonly<Record<string, HistoryKind>> = {
  InitializePrivateBalance: "account",
  DepositCollateral: "deposit",
  WithdrawPublicBalance: "withdraw",
  SubmitQueuedOrder: "order",
  ReplaceQueuedOrder: "replace",
  CancelActiveOrder: "cancel",
  SettleOrderToBalance: "settle",
  RefundOrderToBalance: "refund",
  RolloverOrder: "rollover",
  InitializeAuction: "auction",
};

export type HistoryKind =
  | "account"
  | "deposit"
  | "withdraw"
  | "order"
  | "replace"
  | "cancel"
  | "settle"
  | "refund"
  | "rollover"
  | "auction"
  | "received"
  | "other";

export interface TokenChange {
  readonly mint: string;
  /** Signed decimal string, e.g. "-0.05". */
  readonly amount: string;
}

export interface HistoryEntry {
  readonly signature: string;
  readonly slot: bigint;
  /** Unix seconds, when the RPC knows it. */
  readonly blockTime: number | null;
  readonly kind: HistoryKind;
  readonly failed: boolean;
  /** The owner's public token balance changes in this transaction. */
  readonly tokens: readonly TokenChange[];
  /** The owner's SOL change, fees excluded when the owner paid them. */
  readonly lamports: bigint;
}

interface RpcTokenBalance {
  readonly accountIndex: number;
  readonly mint: string;
  readonly owner?: string;
  readonly uiTokenAmount: {
    readonly amount: string;
    readonly decimals: number;
  };
}

interface RpcTransaction {
  readonly slot: number;
  readonly blockTime: number | null;
  readonly transaction: {
    readonly message: {
      readonly accountKeys: readonly (string | { readonly pubkey: string })[];
    };
  };
  readonly meta: {
    readonly err: unknown;
    readonly fee: number;
    readonly logMessages: readonly string[] | null;
    readonly preBalances: readonly number[];
    readonly postBalances: readonly number[];
    readonly preTokenBalances: readonly RpcTokenBalance[] | null;
    readonly postTokenBalances: readonly RpcTokenBalance[] | null;
  } | null;
}

async function rpcBatch<T>(
  calls: readonly { readonly method: string; readonly params: unknown[] }[],
  signal?: AbortSignal,
): Promise<T[]> {
  if (calls.length === 0) return [];
  const response = await fetch(publicEnv.rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(
      calls.map((call, id) => ({ jsonrpc: "2.0", id, ...call })),
    ),
    signal,
  });
  if (!response.ok) {
    throw new Error(`Solana RPC returned HTTP ${response.status}`);
  }
  const replies = (await response.json()) as {
    readonly id: number;
    readonly result?: T;
    readonly error?: { readonly message: string };
  }[];
  return calls.map((_, id) => {
    const reply = replies.find((candidate) => candidate.id === id);
    if (reply?.error !== undefined) throw new Error(reply.error.message);
    return reply?.result as T;
  });
}

/** Signed decimal string for `atoms` at `decimals` places, trimmed. */
function toDecimal(atoms: bigint, decimals: number): string {
  const sign = atoms < 0n ? "-" : "";
  const magnitude = atoms < 0n ? -atoms : atoms;
  const whole = magnitude / 10n ** BigInt(decimals);
  const fraction = (magnitude % 10n ** BigInt(decimals))
    .toString()
    .padStart(decimals, "0")
    .replace(/0+$/, "");
  return `${sign}${whole}${fraction === "" ? "" : `.${fraction}`}`;
}

/** Classify one transaction from the owner's point of view. */
export function describeTransaction(
  owner: string,
  signature: string,
  transaction: RpcTransaction,
): HistoryEntry {
  const meta = transaction.meta;
  const logs = meta?.logMessages ?? [];
  const actions = logs
    .map((line) => line.match(/^Program log: Instruction: (\w+)$/)?.[1])
    .flatMap((name) =>
      name !== undefined && name in BEATEX_ACTIONS
        ? [BEATEX_ACTIONS[name]!]
        : [],
    );

  const tokens = new Map<string, { atoms: bigint; decimals: number }>();
  const tally = (balances: readonly RpcTokenBalance[] | null, sign: bigint) => {
    for (const balance of balances ?? []) {
      if (balance.owner !== owner) continue;
      const current = tokens.get(balance.mint) ?? {
        atoms: 0n,
        decimals: balance.uiTokenAmount.decimals,
      };
      current.atoms += sign * BigInt(balance.uiTokenAmount.amount);
      tokens.set(balance.mint, current);
    }
  };
  tally(meta?.preTokenBalances ?? null, -1n);
  tally(meta?.postTokenBalances ?? null, 1n);
  const changes = [...tokens.entries()]
    .filter(([, change]) => change.atoms !== 0n)
    .map(([mint, change]) => ({
      mint,
      amount: toDecimal(change.atoms, change.decimals),
    }));

  const keys = transaction.transaction.message.accountKeys.map((key) =>
    typeof key === "string" ? key : key.pubkey,
  );
  const index = keys.indexOf(owner);
  let lamports = 0n;
  if (index !== -1 && meta !== null) {
    lamports =
      BigInt(meta.postBalances[index] ?? 0) -
      BigInt(meta.preBalances[index] ?? 0);
    // The fee payer is always the first key; show what moved, not the fee.
    if (index === 0) lamports += BigInt(meta.fee);
  }

  // A trader's own transaction is named by its BeatEx action; an order that
  // also opened its batch is still an order.
  const kind: HistoryKind =
    actions.find((action) => action !== "auction") ??
    actions[0] ??
    (changes.some((change) => !change.amount.startsWith("-")) || lamports > 0n
      ? "received"
      : "other");

  return {
    signature,
    slot: BigInt(transaction.slot),
    blockTime: transaction.blockTime,
    kind,
    failed: meta?.err !== null && meta?.err !== undefined,
    tokens: changes,
    lamports,
  };
}

/** The owner's most recent transactions, newest first, classified. */
export async function fetchWalletHistory(
  owner: string,
  limit = 25,
  signal?: AbortSignal,
): Promise<HistoryEntry[]> {
  const [signatures] = await rpcBatch<{ readonly signature: string }[]>(
    [
      {
        method: "getSignaturesForAddress",
        params: [owner, { limit, commitment: "confirmed" }],
      },
    ],
    signal,
  );
  const list = signatures ?? [];
  const transactions = await rpcBatch<RpcTransaction | null>(
    list.map(({ signature }) => ({
      method: "getTransaction",
      params: [
        signature,
        {
          encoding: "json",
          commitment: "confirmed",
          maxSupportedTransactionVersion: 1,
        },
      ],
    })),
    signal,
  );
  return list.flatMap(({ signature }, index) => {
    const transaction = transactions[index];
    return transaction === null || transaction === undefined
      ? []
      : [describeTransaction(owner, signature, transaction)];
  });
}
