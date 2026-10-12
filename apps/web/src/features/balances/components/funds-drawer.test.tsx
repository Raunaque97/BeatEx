import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { MarketConfig } from "@/lib/config/markets";

import type { PrivateStateView } from "../hooks/use-private-state";
import { FundsDrawer } from "./funds-drawer";

const mocks = vi.hoisted(() => ({ deposit: vi.fn(), withdraw: vi.fn() }));

vi.mock("@/providers/beatex-provider", () => ({
  useBeatEx: () => ({ client: mocks, owner: "owner" }),
  useTrackedAction: () => ({
    progress: { phase: "idle" },
    busy: false,
    reset: vi.fn(),
    run: (action: (options: object) => Promise<unknown>) => action({}),
  }),
}));
vi.mock("../hooks/use-wallet-balances", () => ({
  useWalletTokens: () => ({ data: new Map() }),
  useWalletBalances: () => ({
    data: {
      lamports: 50_000_000n,
      baseAtoms: 5_000_000n,
      quoteAtoms: 5_000_000_000n,
      baseAccount: "base-ata",
      quoteAccount: "quote-ata",
    },
  }),
}));
vi.mock("../hooks/use-faucet", () => ({
  useFaucet: () => ({ status: "idle", message: null, request: vi.fn() }),
}));

const market: MarketConfig = {
  address: "market",
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
};

beforeEach(() => vi.clearAllMocks());
afterEach(cleanup);

function show() {
  render(
    <FundsDrawer
      market={market}
      privateState={{ status: "none" } as PrivateStateView}
      funds={{ markets: [], tokens: new Map(), complete: true }}
      mints={{ base: "tbtc-mint", quote: "tusd-mint" }}
      initialTab="deposit"
      onClose={vi.fn()}
    />,
  );
}

describe("deposit from wallet", () => {
  it("starts on one token at its full wallet balance", () => {
    show();
    expect(screen.getByRole("button", { name: "Token: tUSD" })).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Deposit 5,000 tUSD" }),
    ).toBeEnabled();
  });

  it("deposits only the chosen token, sized by the slider", async () => {
    show();
    fireEvent.change(screen.getByLabelText("Share of available balance"), {
      target: { value: "50" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Deposit 2,500 tUSD" }));
    await waitFor(() =>
      expect(mocks.deposit).toHaveBeenCalledWith({
        market: "market",
        baseSource: "base-ata",
        quoteSource: "quote-ata",
        baseAmount: 0n,
        quoteAmount: 2_500_000_000n,
        proofOptions: {},
      }),
    );
  });

  it("deposits a typed amount of the other token", async () => {
    show();
    fireEvent.click(screen.getByRole("button", { name: "Token: tUSD" }));
    // Every token is listed with what's available to deposit.
    expect(screen.getByRole("option", { name: "tBTC 0.05" })).toBeVisible();
    expect(screen.getByRole("option", { name: "tUSD 5,000" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    fireEvent.click(screen.getByRole("option", { name: "tBTC 0.05" }));
    expect(screen.queryByRole("listbox")).toBeNull();
    fireEvent.change(screen.getByLabelText("tBTC amount"), {
      target: { value: "0.01" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Deposit 0.01 tBTC" }));
    await waitFor(() =>
      expect(mocks.deposit).toHaveBeenCalledWith(
        expect.objectContaining({ baseAmount: 1_000_000n, quoteAmount: 0n }),
      ),
    );
  });

  it("refuses more than the wallet holds, and nothing at all", () => {
    show();
    fireEvent.change(screen.getByLabelText("tUSD amount"), {
      target: { value: "6000" },
    });
    expect(
      screen.getByText("More than your wallet holds."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Deposit 6,000 tUSD" }),
    ).toBeDisabled();
    fireEvent.change(screen.getByLabelText("tUSD amount"), {
      target: { value: "" },
    });
    expect(
      screen.getByRole("button", { name: "Enter an amount" }),
    ).toBeDisabled();
  });

  it("picks a token from the keyboard", () => {
    show();
    const trigger = screen.getByRole("button", { name: "Token: tUSD" });
    fireEvent.keyDown(trigger, { key: "ArrowDown" });
    fireEvent.keyDown(trigger, { key: "ArrowUp" });
    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(screen.getByRole("button", { name: "Token: tBTC" })).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Deposit 0.05 tBTC" }),
    ).toBeEnabled();
  });
});
