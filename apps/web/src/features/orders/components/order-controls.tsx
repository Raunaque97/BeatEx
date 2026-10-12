"use client";

import type { AuctionGridLevel } from "@beatex/sdk";
import type { KeyboardEvent, PointerEvent } from "react";

import type { MarketConfig } from "@/lib/config/markets";
import { sanitizeDecimal } from "@/lib/decimal-input";
import {
  formatBaseAtoms,
  formatBps,
  formatPrice,
  formatQuoteShort,
} from "@/lib/format";

import type { AmountUnit, OrderSide, OrderType } from "../model/order-curve";

const TYPE_LABELS: Record<OrderType, string> = {
  market: "Market",
  limit: "Limit",
  custom: "Custom",
};

export function OrderTypeTabs({
  types,
  value,
  onChange,
}: {
  readonly types: readonly OrderType[];
  readonly value: OrderType;
  readonly onChange: (type: OrderType) => void;
}) {
  return (
    <div role="tablist" aria-label="Order type" className="flex gap-1">
      {types.map((type) => (
        <button
          key={type}
          type="button"
          role="tab"
          aria-selected={value === type}
          onClick={() => onChange(type)}
          className={
            value === type
              ? "border-line-strong bg-raised min-h-9 rounded-full border px-4 text-sm font-semibold"
              : "text-muted hover:text-text min-h-9 rounded-full border border-transparent px-4 text-sm"
          }
        >
          {TYPE_LABELS[type]}
        </button>
      ))}
    </div>
  );
}

export function SideToggle({
  value,
  onChange,
}: {
  readonly value: OrderSide;
  readonly onChange: (side: OrderSide) => void;
}) {
  const base = "min-h-9 rounded-full border px-4 text-sm font-semibold";
  return (
    <div role="group" aria-label="Side" className="flex gap-1">
      <button
        type="button"
        aria-pressed={value === "buy"}
        onClick={() => onChange("buy")}
        className={`${base} ${value === "buy" ? "border-buy bg-buy-deep text-buy-soft" : "border-line-strong text-muted"}`}
      >
        Buy
      </button>
      <button
        type="button"
        aria-pressed={value === "sell"}
        onClick={() => onChange("sell")}
        className={`${base} ${value === "sell" ? "border-sell bg-sell-deep text-sell-soft" : "border-line-strong text-muted"}`}
      >
        Sell
      </button>
    </div>
  );
}

export function AmountField({
  id,
  label,
  amount,
  unit,
  market,
  size = "large",
  onAmount,
  onUnit,
}: {
  readonly id: string;
  readonly label: string;
  readonly amount: string;
  readonly unit: AmountUnit;
  readonly market: MarketConfig;
  readonly size?: "large" | "compact";
  readonly onAmount: (amount: string) => void;
  readonly onUnit: (unit: AmountUnit) => void;
}) {
  const symbol = unit === "quote" ? market.quote.symbol : market.base.symbol;
  const other = unit === "quote" ? market.base.symbol : market.quote.symbol;
  return (
    <div
      className={
        size === "large" ? "flex flex-col gap-1.5" : "flex items-center gap-3"
      }
    >
      <label htmlFor={id} className="text-muted text-sm whitespace-nowrap">
        {label}
      </label>
      <div className="flex min-w-0 flex-1 items-center gap-3">
        <input
          id={id}
          type="text"
          inputMode="decimal"
          autoComplete="off"
          placeholder="0"
          value={amount}
          onChange={(event) =>
            onAmount(
              sanitizeDecimal(
                event.target.value,
                unit === "quote"
                  ? market.quote.decimals
                  : // Orders trade whole 100-satoshi base units.
                    market.base.decimals - 2,
              ),
            )
          }
          className={`placeholder:text-faint min-w-0 flex-1 bg-transparent font-mono outline-none focus-visible:outline-none ${size === "large" ? "h-12 text-4xl" : "h-9 text-right text-xl"}`}
        />
        <button
          type="button"
          onClick={() => onUnit(unit === "quote" ? "base" : "quote")}
          aria-label={`Enter amount in ${other} instead`}
          className="border-line-strong bg-raised min-h-9 shrink-0 rounded-full border px-3.5 text-sm font-semibold"
        >
          {symbol} <span className="text-muted">⇄</span>
        </button>
      </div>
    </div>
  );
}

/** A horizontal 17-level strip: drag or use arrow keys to choose the limit level. */
export function LimitStrip({
  grid,
  side,
  limitIndex,
  stepBps,
  onChange,
}: {
  readonly grid: readonly AuctionGridLevel[];
  readonly side: OrderSide;
  readonly limitIndex: number;
  readonly stepBps: number;
  readonly onChange: (index: number) => void;
}) {
  const level = grid[limitIndex];
  const pick = (event: PointerEvent<HTMLDivElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const fraction = (event.clientX - rect.left) / rect.width;
    onChange(Math.floor(Math.max(0, Math.min(0.9999, fraction)) * grid.length));
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowRight" || event.key === "ArrowUp")
      onChange(limitIndex + 1);
    else if (event.key === "ArrowLeft" || event.key === "ArrowDown")
      onChange(limitIndex - 1);
    else return;
    event.preventDefault();
  };
  const center = (grid.length - 1) / 2;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-baseline justify-between">
        <span className="text-muted text-sm">
          {side === "buy" ? "Buy at or below" : "Sell at or above"}
        </span>
        <span className="font-mono text-xl">
          {level === undefined ? "—" : formatPrice(level.price)}{" "}
          <span className="text-muted text-xs">
            {formatBps(limitIndex - center, stepBps)}
          </span>
        </span>
      </div>
      <div
        role="slider"
        tabIndex={0}
        aria-label="Limit price"
        aria-valuemin={0}
        aria-valuemax={grid.length - 1}
        aria-valuenow={limitIndex}
        aria-valuetext={
          level === undefined ? undefined : formatPrice(level.price)
        }
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId);
          pick(event);
        }}
        onPointerMove={(event) => {
          if (event.buttons === 1) pick(event);
        }}
        onKeyDown={onKeyDown}
        className="grid h-9 cursor-ew-resize touch-none items-end gap-[3px] rounded"
        style={{
          gridTemplateColumns: `repeat(${grid.length}, minmax(0, 1fr))`,
        }}
      >
        {grid.map((tick) => {
          const selected = tick.index === limitIndex;
          const inRange =
            side === "buy"
              ? tick.index <= limitIndex
              : tick.index >= limitIndex;
          return (
            <span
              key={tick.index}
              className={`rounded-sm ${selected ? "bg-accent h-9" : tick.index === center ? "h-6" : "h-4"} ${
                selected
                  ? ""
                  : inRange
                    ? side === "buy"
                      ? "bg-buy/60"
                      : "bg-sell/60"
                    : "bg-line"
              }`}
            />
          );
        })}
      </div>
      <div className="text-faint flex justify-between font-mono text-[11px]">
        <span>{formatPrice(grid[0]!.price)}</span>
        <span>reference</span>
        <span>{formatPrice(grid[grid.length - 1]!.price)}</span>
      </div>
    </div>
  );
}

export function ActionButton({
  label,
  hint,
  disabled,
  onClick,
}: {
  readonly label: string;
  readonly hint?: string | null;
  readonly disabled?: boolean;
  readonly onClick?: () => void;
}) {
  return (
    <div className="flex flex-col gap-2">
      <button
        type="button"
        disabled={disabled}
        onClick={onClick}
        className="bg-accent text-accent-ink min-h-14 w-full rounded-2xl text-[17px] font-semibold transition-opacity disabled:cursor-not-allowed disabled:opacity-45"
      >
        {label}
      </button>
      {hint ? (
        <p className="text-muted px-1 text-center text-xs">{hint}</p>
      ) : null}
    </div>
  );
}

/** The BeatEx balance of the asset being spent, under the amount field. */
export function BalanceLine({
  market,
  side,
  balance,
}: {
  readonly market: MarketConfig;
  readonly side: OrderSide;
  /** Token atoms; null while the balance is unknown. */
  readonly balance: {
    readonly baseAtoms: bigint;
    readonly quoteAtoms: bigint;
  } | null;
}) {
  if (balance === null) return null;
  const label =
    side === "buy"
      ? `${formatQuoteShort(balance.quoteAtoms)} ${market.quote.symbol}`
      : `${formatBaseAtoms(balance.baseAtoms)} ${market.base.symbol}`;
  return (
    <p className="text-muted text-xs">
      BeatEx balance <span className="text-text font-mono">{label}</span>
    </p>
  );
}
