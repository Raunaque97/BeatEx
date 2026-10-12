import { buildAuctionGrid } from "@beatex/sdk";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import React from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CurrentBatch } from "@/features/auctions/model/current-batch";
import type { PrivateStateView } from "@/features/balances/hooks/use-private-state";
import type { MarketConfig } from "@/lib/config/markets";

import { MainAction } from "./main-action";

const mocks = vi.hoisted(() => ({
  placeOrder: vi.fn(),
  replaceOrder: vi.fn(),
  getAuction: vi.fn(),
  initializeCurrentAuction: vi.fn(),
  getCurrentAuctionId: vi.fn(),
  moveBetweenMarkets: vi.fn(),
  deposit: vi.fn(),
  wallet: vi.fn(),
  openDeposit: vi.fn(),
}));

vi.mock("@/providers/beatex-provider", () => ({
  useBeatEx: () => ({
    owner: "owner",
    walletError: null,
    client: mocks,
  }),
  useTrackedAction: () => ({
    progress: { phase: "idle" },
    busy: false,
    run: (action: (options: object) => Promise<unknown>) => action({}),
  }),
}));
vi.mock("@/features/balances/hooks/use-wallet-balances", () => ({
  useWalletBalances: () => mocks.wallet(),
}));
vi.mock("@/features/balances/hooks/use-faucet", () => ({
  useFaucet: () => ({ status: "idle" }),
}));

const market: MarketConfig = {
  address: "market",
  slug: "tbtc-tusd",
  label: "tBTC / tUSD",
  base: { symbol: "tBTC", decimals: 8 },
  quote: { symbol: "tUSD", decimals: 6 },
  faucet: true,
};
const grid = buildAuctionGrid({
  referencePrice: 85_500n,
  gridStepBps: 10,
  gridSize: 17,
});
const batch = {
  auctionId: 5n,
  phase: "collecting",
  slotsLeft: 30n,
  grid,
  referencePrice: 85_500n,
  auction: { gridStepBps: 10 },
  gridIsPreview: false,
} as CurrentBatch;
const MINTS = { base: "tbtc-mint", quote: "tusd-mint" };
const idleSettlement = {
  state: { status: "idle" },
  progress: { phase: "idle" },
  outcomes: [],
  dismiss: vi.fn(),
} as const;
const order = {
  curve: {
    buy: Array(17).fill(1n) as bigint[],
    sell: Array(17).fill(0n) as bigint[],
  },
  baseReserve: 0n,
  quoteReserve: 86_184n,
};
const privateState = {
  status: "ready",
  record: {
    state: {
      balance: { baseValue: 0n, quoteValue: 1_000_000n },
      activeOrder: null,
    },
  },
} as PrivateStateView;

function show(
  state = privateState,
  currentBatch = batch,
  settlement: Parameters<typeof MainAction>[0]["settlement"] = idleSettlement,
  funds: Parameters<typeof MainAction>[0]["funds"] = new Map(),
) {
  return render(
    <MainAction
      market={market}
      privateState={state}
      orderLabel="Buy tBTC"
      orderError={null}
      order={order}
      batch={currentBatch}
      onDeposit={mocks.openDeposit}
      settlement={settlement}
      funds={funds}
      mints={MINTS}
    />,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.getCurrentAuctionId.mockResolvedValue(5n);
  mocks.wallet.mockReturnValue({
    data: { lamports: 1_000_000_000n, baseAtoms: 0n, quoteAtoms: 0n },
  });
  mocks.getAuction.mockResolvedValue({
    referencePrice: 85_500n,
    gridStepBps: 10,
  });
  mocks.placeOrder.mockResolvedValue({});
});
afterEach(cleanup);

describe("primary order action", () => {
  it("allows a funded buy and submits the composed curve", async () => {
    show();
    expect(screen.getByRole("button", { name: "Buy tBTC" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Buy tBTC" }));
    await waitFor(() =>
      expect(mocks.placeOrder).toHaveBeenCalledWith({
        market: "market",
        curve: order.curve,
        proofOptions: {},
        initializeAuction: false,
      }),
    );
    // The crank creates auctions; a trader never pays for one.
    expect(mocks.initializeCurrentAuction).not.toHaveBeenCalled();
  });

  it("waits for the crank to open the auction before ordering", async () => {
    mocks.getAuction
      .mockResolvedValueOnce(null)
      .mockResolvedValue({ referencePrice: 85_500n, gridStepBps: 10 });
    show();
    fireEvent.click(screen.getByRole("button", { name: "Buy tBTC" }));
    await waitFor(() => expect(mocks.placeOrder).toHaveBeenCalled(), {
      timeout: 2_000,
    });
    expect(mocks.getAuction).toHaveBeenCalledTimes(2);
  });

  it("holds orders in the last seconds of a batch", () => {
    show(privateState, { ...batch, slotsLeft: 4n });
    expect(screen.getByRole("button", { name: "Buy tBTC" })).toBeDisabled();
  });

  it("compares a sell's base reserve with the balance in satoshis", () => {
    const sell = { ...order, baseReserve: 5n, quoteReserve: 0n };
    const funded = (baseValue: bigint) =>
      ({
        status: "ready",
        record: {
          state: { balance: { baseValue, quoteValue: 0n }, activeOrder: null },
        },
      }) as PrivateStateView;
    // 5 base units are 500 satoshis.
    const { unmount } = render(
      <MainAction
        market={market}
        privateState={funded(499n)}
        orderLabel="Sell tBTC"
        orderError={null}
        order={sell}
        batch={batch}
        onDeposit={mocks.openDeposit}
        settlement={idleSettlement}
        funds={new Map()}
        mints={MINTS}
      />,
    );
    expect(
      screen.getByText("Deposit more tBTC for this order."),
    ).toBeInTheDocument();
    unmount();
    render(
      <MainAction
        market={market}
        privateState={funded(500n)}
        orderLabel="Sell tBTC"
        orderError={null}
        order={sell}
        batch={batch}
        onDeposit={mocks.openDeposit}
        settlement={idleSettlement}
        funds={new Map()}
        mints={MINTS}
      />,
    );
    expect(screen.getByRole("button", { name: "Sell tBTC" })).toBeEnabled();
  });

  it("prevents an order exceeding its private collateral", () => {
    show({
      status: "ready",
      record: {
        state: {
          balance: { baseValue: 0n, quoteValue: 1n },
          activeOrder: null,
        },
      },
    } as PrivateStateView);
    expect(screen.getByRole("button", { name: "Buy tBTC" })).toBeDisabled();
    expect(
      screen.getByText("Deposit more tUSD for this order."),
    ).toBeInTheDocument();
  });

  it("does not submit an order built against a different auction grid", async () => {
    mocks.getAuction.mockResolvedValue({
      referencePrice: 86_000n,
      gridStepBps: 10,
    });
    show();
    fireEvent.click(screen.getByRole("button", { name: "Buy tBTC" }));
    await waitFor(() => expect(mocks.getAuction).toHaveBeenCalled());
    expect(mocks.placeOrder).not.toHaveBeenCalled();
    expect(mocks.replaceOrder).not.toHaveBeenCalled();
  });

  it("sends a funded wallet to the deposit slider before the first trade", () => {
    mocks.wallet.mockReturnValue({
      data: {
        lamports: 50_000_000n,
        baseAtoms: 5_000_000n,
        quoteAtoms: 5_000_000_000n,
      },
    });
    show({ status: "none" } as PrivateStateView);
    fireEvent.click(
      screen.getByRole("button", { name: "Deposit from wallet" }),
    );
    expect(mocks.openDeposit).toHaveBeenCalled();
    expect(mocks.deposit).not.toHaveBeenCalled();
  });

  describe("an order from an earlier batch", () => {
    const withOrder = {
      status: "ready",
      record: {
        state: {
          balance: { baseValue: 0n, quoteValue: 1_000_000n },
          activeOrder: { auctionId: 4n, baseReserve: 0n, quoteReserve: 490n },
        },
      },
    } as PrivateStateView;

    it("explains that it settles on its own instead of offering a replace", () => {
      show(withOrder, batch, {
        ...idleSettlement,
        state: { status: "waiting", auctionId: 4n },
      });
      expect(
        screen.getByRole("button", { name: "Order in auction #4" }),
      ).toBeDisabled();
      expect(
        screen.getByText(/settles into your balance automatically/),
      ).toBeInTheDocument();
    });

    it("settles quietly: the order button stays, with a short note", () => {
      show(withOrder, batch, {
        ...idleSettlement,
        state: { status: "settling", auctionId: 4n },
        progress: { phase: "proving" },
      });
      expect(screen.queryByText(/Prove/)).toBeNull();
      expect(
        screen.getByRole("button", { name: "Order in auction #4" }),
      ).toBeInTheDocument();
      expect(
        screen.getByText("Settling your order from auction #4…"),
      ).toBeInTheDocument();
    });

    it("stacks settled results, newest first, each dismissible", () => {
      const dismiss = vi.fn();
      show(privateState, batch, {
        ...idleSettlement,
        dismiss,
        outcomes: [
          {
            auctionId: 5n,
            kind: "bought",
            summary: "Auction #5 filled: bought 0.0012 tBTC for 99.95 tUSD.",
          },
          {
            auctionId: 4n,
            kind: "unfilled",
            summary:
              "Auction #4 didn't fill your order. 490 tUSD returned to your balance.",
          },
        ],
      });
      const banners = screen.getAllByRole("status");
      expect(banners).toHaveLength(2);
      expect(banners[0]).toHaveTextContent("Auction #5 filled");
      fireEvent.click(
        screen.getByRole("button", { name: "Dismiss auction #4 result" }),
      );
      expect(dismiss).toHaveBeenCalledWith(4n);
      expect(screen.getByRole("button", { name: "Buy tBTC" })).toBeEnabled();
    });
  });

  it("moves another market's whole tUSD balance here, then places the order", async () => {
    const beatMarket = { ...market, address: "beat-market", slug: "beat-tusd" };
    // No balance in this market yet: moving funds in opens one.
    const broke = { status: "none" } as PrivateStateView;
    mocks.moveBetweenMarkets.mockResolvedValue({});
    show(
      broke,
      batch,
      idleSettlement,
      new Map([
        [
          "tusd-mint",
          {
            mint: "tusd-mint",
            symbol: "tUSD",
            decimals: 6,
            quote: true,
            free: 6_000_000n,
            locked: 0n,
            byMarket: [
              { market: beatMarket, free: 6_000_000n },
              { market, free: 0n },
            ],
          },
        ],
      ]),
    );
    fireEvent.click(screen.getByRole("button", { name: "Buy tBTC" }));
    await waitFor(() => expect(mocks.placeOrder).toHaveBeenCalled());
    expect(mocks.moveBetweenMarkets).toHaveBeenCalledWith({
      from: "beat-market",
      to: "market",
      mint: "tusd-mint",
      // The whole free balance, not just the order's 86,184 atoms.
      amount: 6_000_000n,
      proofOptions: {},
    });
    expect(mocks.moveBetweenMarkets.mock.invocationCallOrder[0]!).toBeLessThan(
      mocks.placeOrder.mock.invocationCallOrder[0]!,
    );
  });
});
