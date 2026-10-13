"use client";

import { PRICE_LEVELS, type AuctionGridLevel } from "@beatex/sdk";
import { useCallback, useMemo, useState } from "react";

import {
  CENTER_INDEX,
  buildOrder,
  type AmountUnit,
  type OrderIntent,
  type OrderSide,
  type OrderType,
} from "../model/order-curve";

/** A market-maker V: more size further from the reference, 100% at the edges. */
function defaultCustomLevels(): number[] {
  return Array.from({ length: PRICE_LEVELS }, (_, index) => {
    const percent = Math.round(
      (Math.abs(index - CENTER_INDEX) * 100) / CENTER_INDEX,
    );
    if (index < CENTER_INDEX) return percent;
    if (index > CENTER_INDEX) return -percent;
    return 0;
  });
}

const INITIAL_INTENT: OrderIntent = {
  type: "market",
  side: "buy",
  amount: "1000",
  unit: "quote",
  limitIndex: CENTER_INDEX - 1,
  customLevels: defaultCustomLevels(),
  maxBuy: "0.1",
  maxSell: "0.1",
};

/** The order being composed, shared by the Simple and Advanced views. */
export function useOrderIntent(
  grid: readonly AuctionGridLevel[] | null,
  maxOrderQuantity: bigint | undefined,
) {
  const [intent, setIntent] = useState<OrderIntent>(INITIAL_INTENT);

  const setType = useCallback(
    (type: OrderType) => setIntent((current) => ({ ...current, type })),
    [],
  );
  const setSide = useCallback(
    (side: OrderSide) =>
      setIntent((current) => ({
        ...current,
        side,
        // Keep a fresh limit on the passive side of the reference.
        limitIndex:
          side === "buy"
            ? Math.min(current.limitIndex, CENTER_INDEX - 1)
            : Math.max(current.limitIndex, CENTER_INDEX + 1),
      })),
    [],
  );
  const setAmount = useCallback(
    (amount: string) => setIntent((current) => ({ ...current, amount })),
    [],
  );
  const setUnit = useCallback(
    (unit: AmountUnit) =>
      setIntent((current) => ({ ...current, unit, amount: "" })),
    [],
  );
  const setLimitIndex = useCallback(
    (limitIndex: number) =>
      setIntent((current) => ({
        ...current,
        limitIndex: Math.max(0, Math.min(PRICE_LEVELS - 1, limitIndex)),
      })),
    [],
  );
  /** `percent` is signed: positive buys that share of max buy, negative sells. */
  const setCustomLevel = useCallback(
    (index: number, percent: number) =>
      setIntent((current) => {
        const customLevels = [...current.customLevels];
        customLevels[index] = Math.max(
          -100,
          Math.min(100, Math.round(percent)),
        );
        return { ...current, customLevels };
      }),
    [],
  );
  /** Replace every level at once, e.g. from a preset shape. */
  const setCustomLevels = useCallback(
    (levels: readonly number[]) =>
      setIntent((current) => ({
        ...current,
        customLevels: levels.map((percent) =>
          Math.max(-100, Math.min(100, Math.round(percent))),
        ),
      })),
    [],
  );
  const setMaxBuy = useCallback(
    (maxBuy: string) => setIntent((current) => ({ ...current, maxBuy })),
    [],
  );
  const setMaxSell = useCallback(
    (maxSell: string) => setIntent((current) => ({ ...current, maxSell })),
    [],
  );

  const built = useMemo(
    () =>
      grid === null || maxOrderQuantity === undefined
        ? null
        : buildOrder(intent, grid, maxOrderQuantity),
    [intent, grid, maxOrderQuantity],
  );

  return {
    intent,
    built,
    setType,
    setSide,
    setAmount,
    setUnit,
    setLimitIndex,
    setCustomLevel,
    setCustomLevels,
    setMaxBuy,
    setMaxSell,
  };
}

export type OrderIntentState = ReturnType<typeof useOrderIntent>;
