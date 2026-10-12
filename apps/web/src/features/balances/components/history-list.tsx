"use client";

import {
  auctionIdAtSlot,
  formatDecimalUnits,
  GENERATED_DEPLOYMENT,
  type MarketAccount,
} from "@beatex/sdk";
import { useQuery } from "@tanstack/react-query";

import { publicEnv } from "@/lib/config/public-env";
import { groupDecimal } from "@/lib/format";
import { queryKeys } from "@/lib/query/keys";
import {
  fetchWalletHistory,
  type HistoryEntry,
  type HistoryKind,
} from "@/lib/solana/history";

const LABELS: Record<HistoryKind, string> = {
  account: "Opened BeatEx account",
  deposit: "Deposit",
  withdraw: "Withdrawal",
  order: "Placed order",
  replace: "Replaced order",
  cancel: "Cancelled order",
  settle: "Order settled",
  refund: "Order refunded",
  rollover: "Order rolled over",
  auction: "Opened an auction",
  received: "Received test funds",
  other: "Transaction",
};

const SYMBOLS: Readonly<Record<string, string>> = {
  [GENERATED_DEPLOYMENT.baseMint]: "tBTC",
  [GENERATED_DEPLOYMENT.quoteMint]: "tUSD",
};

function symbolFor(mint: string): string {
  return SYMBOLS[mint] ?? `${mint.slice(0, 4)}…`;
}

function amountText(amount: string): string {
  return groupDecimal(amount.replace(/^-/, ""));
}

function timeAgo(seconds: number | null): string {
  if (seconds === null) return "";
  const elapsed = Math.max(0, Date.now() / 1_000 - seconds);
  if (elapsed < 60) return "just now";
  if (elapsed < 3_600) return `${Math.floor(elapsed / 60)}m ago`;
  if (elapsed < 86_400) return `${Math.floor(elapsed / 3_600)}h ago`;
  return new Date(seconds * 1_000).toLocaleDateString();
}

function detail(
  entry: HistoryEntry,
  schedule: MarketAccount | undefined,
): string {
  const tokens = entry.tokens.map(
    (change) => `${amountText(change.amount)} ${symbolFor(change.mint)}`,
  );
  switch (entry.kind) {
    case "deposit":
    case "withdraw":
      return tokens.join(" · ");
    case "received": {
      const sol =
        entry.lamports > 0n
          ? [`${groupDecimal(formatDecimalUnits(entry.lamports, 9))} SOL`]
          : [];
      return [...tokens, ...sol].join(" · ");
    }
    case "order":
    case "replace": {
      const batch =
        schedule === undefined ? null : auctionIdAtSlot(schedule, entry.slot);
      return `${batch === null ? "Sealed" : `Auction #${batch}`} · size encrypted`;
    }
    case "settle":
    case "refund":
      return "Returned to your BeatEx balance";
    case "account":
      return "Your encrypted balance";
    default:
      return "";
  }
}

/** The trader's recent transactions, newest first, read from the chain. */
export function HistoryList({
  owner,
  schedule,
}: {
  readonly owner: string;
  /** The market schedule, to show which batch an order went into. */
  readonly schedule: MarketAccount | undefined;
}) {
  const history = useQuery({
    queryKey: queryKeys.walletHistory(owner),
    queryFn: ({ signal }) => fetchWalletHistory(owner, 25, signal),
    staleTime: 10_000,
  });

  if (history.isPending) {
    return <p className="text-muted text-sm">Loading your history…</p>;
  }
  if (history.isError) {
    return (
      <div className="flex flex-col gap-2 text-sm">
        <p className="text-warn">Couldn&apos;t load your history.</p>
        <button
          type="button"
          onClick={() => void history.refetch()}
          className="text-accent self-start"
        >
          Try again
        </button>
      </div>
    );
  }
  if (history.data.length === 0) {
    return (
      <p className="text-muted text-sm">
        No transactions yet. Get test funds to start.
      </p>
    );
  }

  return (
    <ol className="flex flex-col">
      {history.data.map((entry) => {
        const text = detail(entry, schedule);
        return (
          <li
            key={entry.signature}
            className="border-line flex items-start justify-between gap-3 border-b py-3 last:border-0"
          >
            <div className="min-w-0">
              <p className="text-sm">
                {LABELS[entry.kind]}
                {entry.failed ? (
                  <span className="text-warn ml-2 text-xs">Failed</span>
                ) : null}
              </p>
              {text === "" ? null : (
                <p className="text-muted mt-0.5 font-mono text-xs">{text}</p>
              )}
            </div>
            <a
              href={`https://explorer.solana.com/tx/${entry.signature}?cluster=${publicEnv.cluster}`}
              target="_blank"
              rel="noreferrer"
              className="text-faint hover:text-text shrink-0 text-xs whitespace-nowrap"
            >
              {timeAgo(entry.blockTime)} ↗
            </a>
          </li>
        );
      })}
    </ol>
  );
}
