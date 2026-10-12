"use client";

import type { MarketAccount } from "@beatex/sdk";
import type { ReactNode } from "react";

import { PercentSlider } from "@/components/percent-slider";
import type { CurrentBatch } from "@/features/auctions/model/current-batch";
import type { MarketConfig } from "@/lib/config/markets";
import { formatBase, formatPrice, formatQuote } from "@/lib/format";

import { spendSizing } from "../hooks/balance-sizing";
import type { OrderIntentState } from "../hooks/use-order-intent";
import { estimateOutcome } from "../model/order-curve";
import {
  AmountField,
  BalanceLine,
  LimitStrip,
  OrderTypeTabs,
  SideToggle,
} from "./order-controls";

export function SwapCard({
  market,
  marketAccount,
  batch,
  order,
  action,
  balance,
  onAdvanced,
}: {
  readonly market: MarketConfig;
  readonly marketAccount: MarketAccount | undefined;
  readonly batch: CurrentBatch | null;
  readonly order: OrderIntentState;
  /** The primary button, supplied by the composition layer. */
  readonly action: ReactNode;
  /** BeatEx balance in token atoms; null while unknown. */
  readonly balance: {
    readonly baseAtoms: bigint;
    readonly quoteAtoms: bigint;
  } | null;
  readonly onAdvanced: () => void;
}) {
  const { intent, built } = order;
  const grid = batch?.grid ?? null;
  const estimate =
    built?.ok && grid !== null
      ? estimateOutcome(built.order, grid, intent.side)
      : null;
  // The slider sizes the order as a share of the BeatEx balance being spent.
  const sizing = spendSizing(order, market, balance);
  const receive = (units: bigint) =>
    intent.side === "buy"
      ? `${formatBase(units)} ${market.base.symbol}`
      : `${formatQuote(units)} ${market.quote.symbol}`;

  return (
    <section
      aria-label="Swap"
      className="border-line bg-panel flex w-full max-w-[460px] flex-col gap-2 rounded-3xl border p-3.5"
    >
      <div className="flex items-center justify-between px-1 pb-1.5">
        <OrderTypeTabs
          types={["market", "limit"]}
          value={intent.type === "custom" ? "market" : intent.type}
          onChange={order.setType}
        />
        <SideToggle value={intent.side} onChange={order.setSide} />
      </div>

      <div className="bg-bg focus-within:border-accent/70 rounded-2xl border border-transparent p-4 transition-colors">
        <AmountField
          id="swap-amount"
          label={intent.side === "buy" ? "Spend" : "Sell"}
          amount={intent.amount}
          unit={intent.unit}
          market={market}
          onAmount={order.setAmount}
          onUnit={order.setUnit}
        />
        <div className="mt-2 flex min-h-5 flex-wrap items-center justify-between gap-x-3 gap-y-1">
          <p className="text-muted font-mono text-[13px]" aria-live="polite">
            {estimate === null
              ? built !== null && !built.ok && intent.amount !== ""
                ? built.error
                : " "
              : `≈ ${receive(estimate.receive)}`}
          </p>
          <BalanceLine market={market} side={intent.side} balance={balance} />
        </div>
        <div className="mt-3">
          <PercentSlider
            label={`Share of your ${sizing.symbol} balance`}
            value={sizing.percent}
            disabled={sizing.available === null || sizing.available === 0n}
            onChange={sizing.size}
          />
        </div>
      </div>

      {intent.type === "limit" &&
      grid !== null &&
      marketAccount !== undefined ? (
        <div className="bg-bg rounded-2xl p-4">
          <LimitStrip
            grid={grid}
            side={intent.side}
            limitIndex={intent.limitIndex}
            stepBps={marketAccount.gridStepBps}
            onChange={order.setLimitIndex}
          />
        </div>
      ) : null}

      <div className="px-1 pt-1">{action}</div>

      <details className="text-muted px-1.5 text-[13px]">
        <summary className="flex min-h-9 cursor-pointer items-center gap-1">
          {batch === null
            ? "Loading auction…"
            : `Auction #${batch.auctionId} · one price for everyone`}
          {grid !== null
            ? ` · ${formatPrice(grid[0]!.price)}–${formatPrice(grid[grid.length - 1]!.price)}`
            : ""}
        </summary>
        <div className="flex flex-col gap-2 pb-2 leading-relaxed">
          <p>
            Your order is encrypted in this browser and stays sealed until the
            auction closes. Everyone in the auction trades at a single clearing
            price inside this band.
          </p>
          {estimate !== null && built?.ok ? (
            <p className="font-mono text-xs">
              Fills at {formatPrice(estimate.priceMin)}
              {estimate.priceMax === estimate.priceMin
                ? ""
                : `–${formatPrice(estimate.priceMax)}`}{" "}
              {market.quote.symbol} per {market.base.symbol} · locks{" "}
              {intent.side === "buy"
                ? `${formatQuote(built.order.quoteReserve)} ${market.quote.symbol}`
                : `${formatBase(built.order.baseReserve)} ${market.base.symbol}`}{" "}
              until it clears
            </p>
          ) : null}
          <button
            type="button"
            onClick={onAdvanced}
            className="text-accent self-start underline-offset-4 hover:underline"
          >
            Shape a custom curve in Advanced →
          </button>
        </div>
      </details>
    </section>
  );
}
