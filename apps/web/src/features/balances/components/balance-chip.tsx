"use client";

import { formatQuote } from "@/lib/format";

/**
 * The header's single portfolio figure: everything deposited in BeatEx across
 * every market, including funds locked in open orders, valued in tUSD.
 * Opens Balances.
 */
export function BalanceChip({
  value,
  empty,
  onOpen,
}: {
  /** Quote atoms; null while any market's balance or price is unknown. */
  readonly value: bigint | null;
  /** Nothing deposited anywhere yet. */
  readonly empty: boolean;
  readonly onOpen: () => void;
}) {
  const label = empty
    ? "Deposit"
    : value === null
      ? "Portfolio"
      : `$${formatQuote(value)}`;

  return (
    <button
      type="button"
      onClick={onOpen}
      aria-label={
        empty
          ? "Open balances to deposit"
          : `Portfolio ${label}. Open balances.`
      }
      className={`flex min-h-10 items-center rounded-lg border px-3.5 text-sm ${
        empty
          ? "border-accent/60 text-accent"
          : "border-line-strong bg-panel font-mono"
      }`}
    >
      {label}
    </button>
  );
}
