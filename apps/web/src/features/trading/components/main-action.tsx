"use client";

import { BASE_UNIT_SIZE, type BeatExClient } from "@beatex/sdk";
import { useEffect, useRef, useState } from "react";

import { TxProgressLine } from "@/components/tx-progress-line";
import type { CurrentBatch } from "@/features/auctions/model/current-batch";
import { useFaucet } from "@/features/balances/hooks/use-faucet";
import type { PrivateStateView } from "@/features/balances/hooks/use-private-state";
import {
  useWalletBalances,
  type WalletMints,
} from "@/features/balances/hooks/use-wallet-balances";
import { ActionButton } from "@/features/orders/components/order-controls";
import type { BuiltOrder } from "@/features/orders/model/order-curve";
import { SOL_TOP_UP_FLOOR_LAMPORTS } from "@/lib/config/faucet";
import type { MarketConfig } from "@/lib/config/markets";
import type { useAutoSettle } from "@/features/trading/hooks/use-auto-settle";
import { describeAmounts } from "@/lib/format";
import { planMoves, type TokenFunds } from "@/lib/funds";
import { useBeatEx, useTrackedAction } from "@/providers/beatex-provider";

/**
 * Stop taking orders this many slots before an auction closes. Building,
 * proving, and simulating an order takes about 2–3 s end to end.
 */
const CLOSING_SLOTS = 12n;
const AUCTION_WAIT_MS = 4_000;
/** After moving funds, how long to wait for an auction that is open long enough. */
const OPEN_AUCTION_WAIT_MS = 25_000;
const AUCTION_POLL_MS = 400;

/**
 * The crank creates each batch's auction just after it opens. Wait for it rather
 * than creating it here: racing the crank fails, and a winning trader would pay
 * the auction's ~0.0376 SOL of rent.
 */
async function waitForAuction(
  client: BeatExClient,
  market: string,
  auctionId: bigint,
) {
  const deadline = Date.now() + AUCTION_WAIT_MS;
  for (;;) {
    const auction = await client.getAuction(market, auctionId);
    if (auction !== null) return auction;
    if (Date.now() >= deadline)
      throw new Error("This auction is still opening. Try again in a moment.");
    await new Promise((resolve) => setTimeout(resolve, AUCTION_POLL_MS));
  }
}

/** Wait until `read()` shows an auction collecting with time to spare. */
async function waitForOpenAuction(read: () => CurrentBatch | null) {
  const deadline = Date.now() + OPEN_AUCTION_WAIT_MS;
  for (;;) {
    const batch = read();
    if (
      batch !== null &&
      batch.grid !== null &&
      batch.phase === "collecting" &&
      batch.slotsLeft >= CLOSING_SLOTS
    ) {
      return;
    }
    if (Date.now() >= deadline)
      throw new Error("No auction opened in time. Try again in a moment.");
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
}

/**
 * The single primary button. It walks a new trader through test funds and a
 * deposit, then becomes the order button. The demo wallet signs without prompts.
 */
export function MainAction({
  market,
  privateState,
  orderLabel,
  orderError,
  order,
  batch,
  onDeposit,
  settlement,
  funds,
  mints,
}: {
  readonly market: MarketConfig;
  readonly privateState: PrivateStateView;
  readonly orderLabel: string;
  readonly orderError: string | null;
  readonly order: BuiltOrder | null;
  readonly batch: CurrentBatch | null;
  /** Open the deposit slider in the Balances panel. */
  readonly onDeposit: () => void;
  /** Background settlement of the previous order, owned by the workspace. */
  readonly settlement: ReturnType<typeof useAutoSettle>;
  /** The trader's funds per token across every market. */
  readonly funds: ReadonlyMap<string, TokenFunds>;
  /** This market's token mints, once its account has loaded. */
  readonly mints: WalletMints | null;
}) {
  const { owner, walletError, client, notifyPrivateChange } = useBeatEx();
  const wallet = useWalletBalances(owner, mints);
  const faucet = useFaucet();
  const [working, setWorking] = useState(false);
  // Fixed at submit time: once confirmed, the new order is itself the active one.
  const [submitted, setSubmitted] = useState<
    "Order" | "Replacement" | "Moving funds"
  >("Order");
  // The newest order and auction, read after a move that outlasted the click.
  const latest = useRef({ order, batch });
  useEffect(() => {
    latest.current = { order, batch };
  });
  const [error, setError] = useState<string | null>(null);
  const action = useTrackedAction();

  const submit = async () => {
    if (
      client === null ||
      order === null ||
      batch === null ||
      action.busy ||
      // "none": no balance here yet; moving funds in opens one.
      (privateState.status !== "ready" && privateState.status !== "none")
    )
      return;
    const active =
      privateState.status === "ready"
        ? privateState.record.state.activeOrder
        : null;
    // A replacement in the same auction releases the old order's reserves.
    const releasing = active !== null && active.auctionId === batch.auctionId;
    const moves =
      mints === null
        ? []
        : [
            ...(
              planMoves(
                funds,
                market.address,
                mints.quote,
                order.quoteReserve - (releasing ? active.quoteReserve : 0n),
              ) ?? []
            ).map((move) => ({ ...move, mint: mints.quote })),
            ...(
              planMoves(
                funds,
                market.address,
                mints.base,
                order.baseReserve * BASE_UNIT_SIZE -
                  (releasing ? active.baseReserve : 0n),
              ) ?? []
            ).map((move) => ({ ...move, mint: mints.base })),
          ];

    let target = { order, batch };
    if (moves.length > 0) {
      // Bring this market's token over from the others first: their whole
      // free balance, so the public move says nothing about the order's size.
      setSubmitted("Moving funds");
      try {
        for (const move of moves) {
          await action.run((proofOptions) =>
            client.moveBetweenMarkets({
              from: move.from.address,
              to: market.address,
              mint: move.mint,
              amount: move.amount,
              proofOptions,
            }),
          );
        }
        // The move takes a few seconds; place into whichever auction is open now.
        await waitForOpenAuction(() => latest.current.batch);
      } catch {
        // Shown by the progress line.
        return;
      }
      const next = latest.current;
      if (next.order === null || next.batch === null) return;
      target = { order: next.order, batch: next.batch };
    }
    await placeInto(target.order, target.batch, active);
  };

  const placeInto = async (
    order: BuiltOrder,
    batch: CurrentBatch,
    active: NonNullable<
      Extract<
        PrivateStateView,
        { status: "ready" }
      >["record"]["state"]["activeOrder"]
    > | null,
  ) => {
    if (client === null) return;
    setSubmitted(active === null ? "Order" : "Replacement");
    try {
      await action.run(async (proofOptions) => {
        // Pin the displayed grid to the auction selected for submission. A
        // boundary or a newly snapshotted reference requires another review.
        const auctionId = await client.getCurrentAuctionId(market.address);
        if (auctionId !== batch.auctionId) {
          throw new Error(
            "The auction price grid changed. Review your order and try again.",
          );
        }
        const auction = await waitForAuction(client, market.address, auctionId);
        if (
          auction.referencePrice !== batch.referencePrice ||
          (batch.auction !== null &&
            auction.gridStepBps !== batch.auction.gridStepBps)
        ) {
          throw new Error(
            "The auction price grid changed. Review your order and try again.",
          );
        }
        const params = {
          market: market.address,
          curve: order.curve,
          proofOptions,
        };
        if (active !== null) {
          if (active.auctionId !== auctionId)
            throw new Error(
              "Your previous order must settle before placing a new one.",
            );
          return client.replaceOrder(params);
        }
        return client.placeOrder({ ...params, initializeAuction: false });
      });
    } catch {
      // Proof, wallet, and simulation errors are shown by the progress line.
    }
  };

  const attempt = (task: () => Promise<void>) => async () => {
    setWorking(true);
    setError(null);
    try {
      await task();
    } catch (cause) {
      setError(
        cause instanceof Error ? cause.message : "Something went wrong.",
      );
    } finally {
      setWorking(false);
    }
  };

  if (walletError !== null) {
    return (
      <ActionButton label="Wallet unavailable" disabled hint={walletError} />
    );
  }
  if (owner === null) {
    return <ActionButton label="Creating your demo wallet…" disabled />;
  }

  if (action.busy) {
    return <TxProgressLine title={submitted} progress={action.progress} />;
  }

  switch (privateState.status) {
    case "locked":
    case "loading":
      return <ActionButton label="Loading your balance…" disabled />;
    case "pending":
      return (
        <ActionButton
          label={working ? "Checking…" : "Finishing your last transaction…"}
          disabled={working || client === null}
          onClick={attempt(async () => {
            await client!.recoverPendingTransition(market.address);
            notifyPrivateChange();
          })}
          hint={
            error ??
            "It was sent but not confirmed yet. This resolves on its own; click to check now."
          }
        />
      );
    case "unavailable":
      return (
        <ActionButton
          label="Backup needed"
          disabled
          hint={privateState.reason}
        />
      );
    case "error":
      return (
        <ActionButton
          label="Couldn't load balance"
          disabled
          hint={privateState.message}
        />
      );
    default:
      break;
  }

  const balance =
    privateState.status === "ready" ? privateState.record.state.balance : null;
  // Free funds anywhere count: an order moves them here first.
  const spendableBase =
    (mints === null ? undefined : funds.get(mints.base)?.free) ??
    balance?.baseValue ??
    0n;
  const spendableQuote =
    (mints === null ? undefined : funds.get(mints.quote)?.free) ??
    balance?.quoteValue ??
    0n;
  const hasPrivateFunds =
    [...funds.values()].some((token) => token.free > 0n) ||
    (balance !== null && (balance.baseValue > 0n || balance.quoteValue > 0n));
  const active =
    privateState.status === "ready"
      ? privateState.record.state.activeOrder
      : null;
  const lowOnSol =
    wallet.data !== undefined &&
    wallet.data.lamports < SOL_TOP_UP_FLOOR_LAMPORTS;
  const walletEmpty =
    wallet.data !== undefined &&
    wallet.data.baseAtoms === 0n &&
    wallet.data.quoteAtoms === 0n;
  const getFunds = (
    <ActionButton
      label={
        faucet.status === "sending" ? "Sending test funds…" : "Get test funds"
      }
      disabled={faucet.status === "sending"}
      onClick={() => void faucet.request()}
      hint={
        faucet.message ??
        `Free Devnet ${market.base.symbol}, ${market.quote.symbol}, and SOL for fees.`
      }
    />
  );
  const walletBase = wallet.data?.baseAtoms ?? 0n;
  const walletQuote = wallet.data?.quoteAtoms ?? 0n;
  const walletFunds = describeAmounts(walletBase, walletQuote, {
    base: market.base.symbol,
    quote: market.quote.symbol,
  });
  if (!hasPrivateFunds && active === null) {
    if (walletEmpty || lowOnSol) return getFunds;
    return (
      <ActionButton
        label="Deposit from wallet"
        onClick={onDeposit}
        hint="Trade from your BeatEx balance. Deposits are encrypted, so nobody can see what you trade."
      />
    );
  }
  // Each order holds ~0.015 SOL of account rent while it is open.
  if (lowOnSol && active === null) return getFunds;

  const eligibilityError =
    orderError ??
    (batch === null || batch.grid === null
      ? "Loading the auction price grid…"
      : null) ??
    (active !== null && active.auctionId !== batch?.auctionId
      ? settlement.state.status === "retrying"
        ? `Settling auction #${active.auctionId}: ${settlement.state.message} Retrying…`
        : settlement.state.status === "settling"
          ? `Settling your order from auction #${active.auctionId}…`
          : `Your order is in auction #${active.auctionId}. It settles into your balance automatically when that auction closes.`
      : null) ??
    (batch !== null && batch.phase !== "collecting" && !batch.gridIsPreview
      ? "This auction is closed. Wait for the next one."
      : null) ??
    (batch !== null && batch.slotsLeft < CLOSING_SLOTS
      ? "This auction is closing. Your order can go into the next one in a moment."
      : null) ??
    (order === null ? "Enter an order amount." : null) ??
    (order !== null &&
    // The curve's reserve is in base units; balances and stored reserves are atoms.
    order.baseReserve * BASE_UNIT_SIZE >
      spendableBase + (active?.baseReserve ?? 0n)
      ? `Deposit more ${market.base.symbol} for this order.`
      : null) ??
    (order !== null &&
    order.quoteReserve > spendableQuote + (active?.quoteReserve ?? 0n)
      ? `Deposit more ${market.quote.symbol} for this order.`
      : null);

  return (
    <div className="flex flex-col gap-3">
      {settlement.outcomes.length > 0 ? (
        <ul aria-label="Settled orders" className="flex flex-col gap-2">
          {settlement.outcomes.map((outcome) => (
            <li
              key={outcome.auctionId.toString()}
              role="status"
              className="outcome-in border-accent/30 bg-raised flex items-start justify-between gap-3 rounded-xl border px-3.5 py-2.5 text-sm"
            >
              <span>{outcome.summary}</span>
              <button
                type="button"
                aria-label={`Dismiss auction #${outcome.auctionId} result`}
                onClick={() => settlement.dismiss(outcome.auctionId)}
                className="text-faint hover:text-text -mr-1 leading-none"
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      {/* Once the order has settled, its outcome replaces the confirmation. */}
      {action.progress.phase !== "idle" &&
      !(action.progress.phase === "confirmed" && active === null) ? (
        <TxProgressLine title={submitted} progress={action.progress} />
      ) : null}
      <ActionButton
        label={
          active === null
            ? orderLabel
            : active.auctionId === batch?.auctionId
              ? "Replace current order"
              : `Order in auction #${active.auctionId}`
        }
        disabled={client === null || eligibilityError !== null || working}
        onClick={() => void submit()}
        hint={
          eligibilityError ??
          "Your order stays confidential. Only aggregate curves are published."
        }
      />
      {walletBase > 0n || walletQuote > 0n ? (
        <div className="border-line text-muted flex items-center justify-between gap-3 border-t pt-3 text-xs">
          <span>
            <span className="text-text font-mono">{walletFunds}</span> in your
            wallet
          </span>
          <button
            type="button"
            onClick={onDeposit}
            className="text-accent font-semibold"
          >
            Deposit
          </button>
        </div>
      ) : null}
    </div>
  );
}
