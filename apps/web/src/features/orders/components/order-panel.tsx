"use client";

import type { MarketAccount } from "@beatex/sdk";
import type { ReactNode } from "react";

import type { CurrentBatch } from "@/features/auctions/model/current-batch";
import type { MarketConfig } from "@/lib/config/markets";
import { PercentSlider } from "@/components/percent-slider";
import {
  formatBase,
  formatBaseAtoms,
  formatQuote,
  formatQuoteShort,
} from "@/lib/format";
import type { Holdings } from "@/lib/portfolio";

import { customSizing, spendSizing } from "../hooks/balance-sizing";
import type { OrderIntentState } from "../hooks/use-order-intent";
import { estimateOutcome } from "../model/order-curve";
import { CurveEditor, MaxField } from "./curve-editor";
import { CurvePresets } from "./curve-presets";
import {
  AmountField,
  BalanceLine,
  OrderTypeTabs,
  SideToggle,
} from "./order-controls";

const HINTS = {
  market: "Trades the same size at whichever level the auction clears.",
  limit:
    "Drag the line. Bars show the size you'd trade at each possible clearing price.",
  custom:
    "Drag each level left to sell or right to buy. Only the clearing level executes; you lock the largest level, not the sum.",
} as const;

export function OrderPanel({
  market,
  marketAccount,
  batch,
  order,
  action,
  balance,
}: {
  readonly market: MarketConfig;
  readonly marketAccount: MarketAccount | undefined;
  readonly batch: CurrentBatch | null;
  readonly order: OrderIntentState;
  /** The primary button, supplied by the composition layer. */
  readonly action: ReactNode;
  /** Free BeatEx balance in token atoms; null while unknown. */
  readonly balance: Holdings | null;
}) {
  const { intent, built } = order;
  const grid = batch?.grid ?? null;
  const isCustom = intent.type === "custom";
  const spend = spendSizing(order, market, balance);
  const custom = customSizing(order, balance, grid);
  const estimate =
    !isCustom && built?.ok && grid !== null
      ? estimateOutcome(built.order, grid, intent.side)
      : null;

  const locks: string[] = [];
  if (built?.ok) {
    if (built.order.quoteReserve > 0n) {
      locks.push(
        `${formatQuote(built.order.quoteReserve)} ${market.quote.symbol}`,
      );
    }
    if (built.order.baseReserve > 0n) {
      locks.push(
        `${formatBase(built.order.baseReserve)} ${market.base.symbol}`,
      );
    }
  }

  return (
    <section aria-label="Order" className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <OrderTypeTabs
          types={["market", "limit", "custom"]}
          value={intent.type}
          onChange={order.setType}
        />
        {isCustom ? null : (
          <SideToggle value={intent.side} onChange={order.setSide} />
        )}
      </div>

      {/* One fixed-height block for what the trader sets, so the ladder never moves. */}
      <div className="h-[136px]">
        {isCustom ? (
          <div className="grid h-full grid-cols-2 gap-2">
            {(
              [
                {
                  id: "max-sell",
                  label: "Max sell",
                  value: intent.maxSell,
                  onChange: order.setMaxSell,
                  tone: "sell",
                  sizing: custom.sell,
                  note:
                    custom.sell.available === null
                      ? ""
                      : `Balance ${formatBaseAtoms(custom.sell.available)} ${market.base.symbol}`,
                  slider: `Share of your ${market.base.symbol} balance`,
                },
                {
                  id: "max-buy",
                  label: "Max buy",
                  value: intent.maxBuy,
                  onChange: order.setMaxBuy,
                  tone: "buy",
                  sizing: custom.buy,
                  note:
                    balance === null || custom.buy.available === null
                      ? ""
                      : `${formatQuoteShort(balance.quoteAtoms)} ${market.quote.symbol} ≈ ${formatBase(custom.buy.available)} ${market.base.symbol}`,
                  slider: `Share of what your ${market.quote.symbol} can buy`,
                },
              ] as const
            ).map((field) => (
              <div key={field.id} className="flex min-w-0 flex-col gap-1.5">
                <div className="h-14">
                  <MaxField
                    id={field.id}
                    label={field.label}
                    value={field.value}
                    onChange={field.onChange}
                    tone={field.tone}
                  />
                </div>
                <p className="text-muted truncate font-mono text-[11px]">
                  {field.note}
                </p>
                <PercentSlider
                  label={field.slider}
                  value={field.sizing.percent}
                  disabled={
                    field.sizing.available === null ||
                    field.sizing.available === 0n
                  }
                  onChange={field.sizing.size}
                />
              </div>
            ))}
          </div>
        ) : (
          <div className="flex h-full flex-col gap-1.5">
            <div className="border-line bg-panel focus-within:border-accent/70 flex h-14 items-center rounded-xl border px-3.5 transition-colors">
              <AmountField
                id="advanced-amount"
                label={intent.side === "buy" ? "Spend" : "Sell"}
                amount={intent.amount}
                unit={intent.unit}
                market={market}
                size="compact"
                onAmount={order.setAmount}
                onUnit={order.setUnit}
              />
            </div>
            <div className="flex h-[17px] items-center">
              <BalanceLine
                market={market}
                side={intent.side}
                balance={balance}
              />
            </div>
            <PercentSlider
              label={`Share of your ${spend.symbol} balance`}
              value={spend.percent}
              disabled={spend.available === null || spend.available === 0n}
              onChange={spend.size}
            />
          </div>
        )}
      </div>

      {grid === null || marketAccount === undefined ? (
        <div className="border-line text-muted flex h-[336px] items-center justify-center rounded-xl border border-dashed text-sm">
          Waiting for the auction price grid…
        </div>
      ) : (
        <CurveEditor
          grid={grid}
          stepBps={marketAccount.gridStepBps}
          order={order}
        />
      )}

      {/* Same height in every tab, so the order button never moves. */}
      <div className="flex h-[72px] flex-col gap-2">
        {isCustom ? (
          <CurvePresets
            levels={intent.customLevels}
            onPick={order.setCustomLevels}
          />
        ) : null}
        <p className="text-muted text-xs leading-snug">{HINTS[intent.type]}</p>
      </div>

      <div
        className="text-muted flex justify-between gap-3 font-mono text-xs"
        aria-live="polite"
      >
        <span>
          {built?.ok
            ? `Locks ${locks.join(" + ")}`
            : built !== null && (isCustom || intent.amount !== "")
              ? built.error
              : ""}
        </span>
        <span>
          {estimate === null
            ? ""
            : `≈ ${
                intent.side === "buy"
                  ? `${formatBase(estimate.receive)} ${market.base.symbol}`
                  : `${formatQuote(estimate.receive)} ${market.quote.symbol}`
              }`}
        </span>
      </div>

      {/* Pinned to the bottom of the panel, so the order button is always in
          view; anything that doesn't fit scrolls above it. */}
      <div className="bg-bg sticky bottom-0 -mx-1 px-1 pt-2 pb-1">{action}</div>
    </section>
  );
}
