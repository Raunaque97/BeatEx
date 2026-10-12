import { buildAuctionGrid } from "@beatex/sdk";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import React from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CurrentBatch } from "@/features/auctions/model/current-batch";
import type { MarketConfig } from "@/lib/config/markets";

import type { OrderIntentState } from "../hooks/use-order-intent";
import { SwapCard } from "./swap-card";

const market: MarketConfig = {
  address: "market",
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
};
const grid = buildAuctionGrid({
  referencePrice: 83_000n,
  gridStepBps: 10,
  gridSize: 17,
});

afterEach(cleanup);

function show(side: "buy" | "sell", unit: "quote" | "base", amount = "") {
  const order = {
    intent: {
      type: "market",
      side,
      amount,
      unit,
      limitIndex: 8,
      customLevels: [],
      maxBuy: "",
      maxSell: "",
    },
    built: null,
    setType: vi.fn(),
    setSide: vi.fn(),
    setAmount: vi.fn(),
    setUnit: vi.fn(),
    setLimitIndex: vi.fn(),
  } as unknown as OrderIntentState;
  render(
    <SwapCard
      market={market}
      marketAccount={undefined}
      batch={{ auctionId: 1n, grid } as unknown as CurrentBatch}
      order={order}
      action={null}
      balance={{ baseAtoms: 1_234_567n, quoteAtoms: 4_900_000_000n }}
      onAdvanced={vi.fn()}
    />,
  );
  return order;
}

describe("spend slider", () => {
  it("sizes a buy as a share of the tUSD balance", () => {
    const order = show("buy", "quote");
    fireEvent.click(screen.getByRole("button", { name: "50%" }));
    expect(order.setAmount).toHaveBeenLastCalledWith("2450");
    expect(order.setUnit).not.toHaveBeenCalled();
  });

  it("sizes a sell in whole 100-satoshi units of the tBTC balance", () => {
    const order = show("sell", "base");
    fireEvent.click(screen.getByRole("button", { name: "Max" }));
    // 1,234,567 satoshis round down to 1,234,500.
    expect(order.setAmount).toHaveBeenLastCalledWith("0.012345");
  });

  it("switches to the spent asset when the amount was in the other one", () => {
    const order = show("buy", "base", "0.01");
    fireEvent.click(screen.getByRole("button", { name: "25%" }));
    expect(order.setUnit).toHaveBeenCalledWith("quote");
    expect(order.setAmount).toHaveBeenLastCalledWith("1225");
  });

  it("shows where a typed amount sits on the slider", () => {
    show("buy", "quote", "490");
    expect(
      screen.getByRole("slider", { name: "Share of your tUSD balance" }),
    ).toHaveValue("10");
  });
});
