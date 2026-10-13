"use client";

import type { AuctionGridLevel } from "@beatex/sdk";
import { useState, type KeyboardEvent, type PointerEvent } from "react";

import { sanitizeDecimal } from "@/lib/decimal-input";
import { formatBase, formatBps, formatPrice, formatQuote } from "@/lib/format";

import type { OrderIntentState } from "../hooks/use-order-intent";

const ROW_PX = 18;

type CustomDisplay = "percent" | "amount";

/**
 * The batch's price levels top (highest) to bottom. Bars show the size traded if the
 * batch clears at that level: sells extend left of zero, buys right. Custom orders
 * drag each bar; limit orders drag a line across the ladder.
 */
export function CurveEditor({
  grid,
  stepBps,
  order,
}: {
  readonly grid: readonly AuctionGridLevel[];
  readonly stepBps: number;
  readonly order: OrderIntentState;
}) {
  const { intent, built } = order;
  const isCustom = intent.type === "custom";
  const isLimit = intent.type === "limit";
  const center = (grid.length - 1) / 2;
  const [display, setDisplay] = useState<CustomDisplay>("percent");

  // Signed base units per level from the built order (buys positive).
  const signed = grid.map((level) => {
    if (!built?.ok) return 0n;
    const { buy, sell } = built.order.curve;
    return buy[level.index]! - sell[level.index]!;
  });
  const largest = signed.reduce((max, value) => {
    const magnitude = value < 0n ? -value : value;
    return magnitude > max ? magnitude : max;
  }, 0n);
  const scale = largest === 0n ? 1n : largest;
  const showAmounts = !isCustom || (display === "amount" && built?.ok === true);

  const indexAt = (event: PointerEvent<HTMLDivElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const fraction = Math.max(
      0,
      Math.min(0.9999, (event.clientY - rect.top) / rect.height),
    );
    return grid.length - 1 - Math.floor(fraction * grid.length);
  };
  const onLimitKey = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "ArrowUp") order.setLimitIndex(intent.limitIndex + 1);
    else if (event.key === "ArrowDown")
      order.setLimitIndex(intent.limitIndex - 1);
    else return;
    event.preventDefault();
  };

  const rows = [...grid].reverse();
  const limitTop = (grid.length - 1 - intent.limitIndex) * ROW_PX;

  return (
    <div className="flex flex-col gap-1.5">
      <div className="text-muted grid h-6 grid-cols-[64px_minmax(0,1fr)_76px] items-center gap-2 font-mono text-[11px]">
        <span>Price</span>
        <span className="flex justify-between">
          <span className="text-sell-soft">← Sell</span>
          <span>0</span>
          <span className="text-buy-soft">Buy →</span>
        </span>
        {isCustom ? (
          <span
            role="group"
            aria-label="Show levels as"
            className="border-line flex justify-self-end rounded border"
          >
            {(["percent", "amount"] as const).map((option) => (
              <button
                key={option}
                type="button"
                aria-pressed={display === option}
                onClick={() => setDisplay(option)}
                className={`h-5 px-1.5 leading-5 ${display === option ? "bg-raised text-text" : "text-muted"}`}
              >
                {option === "percent" ? "%" : "tBTC"}
              </button>
            ))}
          </span>
        ) : (
          <span className="text-right">
            {intent.unit === "base" ? "tBTC" : "tUSD"}
          </span>
        )}
      </div>

      <div
        className={`relative touch-none ${isLimit ? "cursor-ns-resize" : ""}`}
        onPointerDown={(event) => {
          if (!isLimit) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          order.setLimitIndex(indexAt(event));
        }}
        onPointerMove={(event) => {
          if (isLimit && event.buttons === 1)
            order.setLimitIndex(indexAt(event));
        }}
      >
        {rows.map((level) => {
          const percent = isCustom
            ? (intent.customLevels[level.index] ?? 0)
            : 0;
          const value = signed[level.index]!;
          const magnitude = value < 0n ? -value : value;
          // Custom bars show the slider position; other bars scale to the largest level.
          const width = isCustom
            ? Math.abs(percent) / 2
            : Number((magnitude * 1000n) / scale) / 20;
          const isBuy = isCustom ? percent > 0 : value > 0n;
          const label = !showAmounts
            ? percent === 0
              ? ""
              : `${Math.abs(percent)}%`
            : value === 0n
              ? ""
              : !isCustom && intent.unit === "quote"
                ? formatQuote(magnitude * level.price).split(".")[0]
                : formatBase(magnitude);
          return (
            <div
              key={level.index}
              className={`grid grid-cols-[64px_minmax(0,1fr)_76px] items-center gap-2 ${level.index === center ? "bg-buy/10" : ""}`}
              style={{ height: ROW_PX }}
            >
              <span
                className={`font-mono text-[11px] ${level.index === center ? "text-text" : "text-muted"}`}
                title={formatBps(level.offset, stepBps)}
              >
                {formatPrice(level.price)}
              </span>
              <div className="has-[:focus-visible]:outline-accent relative h-4 rounded-sm has-[:focus-visible]:outline has-[:focus-visible]:outline-2">
                <span
                  aria-hidden="true"
                  className="bg-line-strong absolute -top-1 -bottom-1 left-1/2 w-px"
                />
                <span
                  aria-hidden="true"
                  className={`absolute top-0.5 h-3 rounded-sm ${isBuy ? "bg-buy left-1/2" : "bg-sell right-1/2"}`}
                  style={{ width: `${width}%` }}
                />
                {isCustom ? (
                  <input
                    type="range"
                    min={-100}
                    max={100}
                    step={1}
                    value={percent}
                    onChange={(event) =>
                      order.setCustomLevel(
                        level.index,
                        Number(event.target.value),
                      )
                    }
                    aria-label={`Size at ${formatPrice(level.price)}: left sells, right buys`}
                    aria-valuetext={
                      percent === 0
                        ? "none"
                        : `${percent > 0 ? "buy" : "sell"} ${Math.abs(percent)}% of max${
                            value === 0n
                              ? ""
                              : `, ${formatBase(magnitude)} tBTC`
                          }`
                    }
                    className="level-slider"
                  />
                ) : null}
              </div>
              <span
                className={`text-right font-mono text-[11px] ${isBuy ? "text-buy-soft" : "text-sell-soft"}`}
              >
                {label}
              </span>
            </div>
          );
        })}

        {isLimit ? (
          <div
            className="border-accent pointer-events-none absolute right-0 left-0 border-t-2"
            style={{ top: limitTop + ROW_PX / 2 }}
          >
            <div
              role="slider"
              tabIndex={0}
              aria-label="Limit price"
              aria-orientation="vertical"
              aria-valuemin={0}
              aria-valuemax={grid.length - 1}
              aria-valuenow={intent.limitIndex}
              aria-valuetext={formatPrice(grid[intent.limitIndex]!.price)}
              onKeyDown={onLimitKey}
              className="bg-accent text-accent-ink pointer-events-auto absolute -top-3 left-0 rounded px-2 py-0.5 font-mono text-[11px] font-semibold"
            >
              {formatPrice(grid[intent.limitIndex]!.price)} ⇕
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}

/** One side's 100% amount for custom curves. */
export function MaxField({
  id,
  label,
  value,
  onChange,
  tone,
}: {
  readonly id: string;
  readonly label: string;
  readonly value: string;
  readonly onChange: (value: string) => void;
  readonly tone: "buy" | "sell";
}) {
  return (
    <div
      className={`bg-panel flex h-full items-center gap-2 rounded-xl border px-3 transition-colors ${tone === "buy" ? "border-buy/40 focus-within:border-buy" : "border-sell/40 focus-within:border-sell"}`}
    >
      <label
        htmlFor={id}
        className={`text-xs whitespace-nowrap ${tone === "buy" ? "text-buy-soft" : "text-sell-soft"}`}
      >
        {label}
      </label>
      <input
        id={id}
        type="text"
        inputMode="decimal"
        autoComplete="off"
        placeholder="0"
        value={value}
        onChange={(event) => onChange(sanitizeDecimal(event.target.value, 6))}
        className="placeholder:text-faint h-7 min-w-0 flex-1 bg-transparent text-right font-mono text-sm outline-none focus-visible:outline-none"
      />
      <span className="text-muted text-xs">tBTC</span>
    </div>
  );
}
