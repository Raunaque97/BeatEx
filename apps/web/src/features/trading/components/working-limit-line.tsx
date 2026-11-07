"use client";

import type { MarketConfig } from "@/lib/config/markets";
import { baseUnitsToTokens, formatPrice } from "@/lib/format";

import type { WorkingLimit } from "../model/working-limit";

const EXPLANATION =
  "This tab resubmits your order to each auction until it has fully filled. Closing the tab or leaving this market cancels what hasn't been submitted.";

/** Where a limit order stands between auctions. */
export function WorkingLimitLine({
  market,
  working,
  onStop,
}: {
  readonly market: MarketConfig;
  readonly working: WorkingLimit;
  readonly onStop: () => void;
}) {
  const percent = (working.filled * 100n) / working.total;
  return (
    <div
      role="status"
      className="border-line bg-raised flex items-center justify-between gap-3 rounded-xl border px-3.5 py-2.5 text-sm"
    >
      <div className="min-w-0">
        <div className="flex items-center gap-1.5">
          <span>
            Limit {working.side}{" "}
            <span className="font-mono">
              {baseUnitsToTokens(working.total, market)} {market.base.symbol}
            </span>{" "}
            at{" "}
            <span className="font-mono">
              {formatPrice(working.limitPrice, market)}
            </span>
          </span>
          <span
            tabIndex={0}
            title={EXPLANATION}
            aria-label={EXPLANATION}
            className="border-line text-faint inline-flex size-4 shrink-0 cursor-help items-center justify-center rounded-full border text-[10px] leading-none"
          >
            i
          </span>
        </div>
        <div className="text-muted text-xs">
          {percent.toString()}% filled ·{" "}
          {working.auctionId === null
            ? "waiting for the next auction"
            : `in auction #${working.auctionId}`}
        </div>
      </div>
      <button
        type="button"
        onClick={onStop}
        className="text-accent shrink-0 text-xs font-semibold"
      >
        Stop
      </button>
    </div>
  );
}
