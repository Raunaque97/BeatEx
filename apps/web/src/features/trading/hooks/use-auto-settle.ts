"use client";

import { AuctionState } from "@beatex/sdk";
import { useCallback, useEffect, useState } from "react";

import type { PrivateStateView } from "@/features/balances/hooks/use-private-state";
import type { MarketConfig } from "@/lib/config/markets";
import { useBeatEx, useTrackedAction } from "@/providers/beatex-provider";

import {
  describeSettlement,
  type SettlementOutcome,
} from "../model/settlement-outcome";

/** How often to look at an open order's batch while it is still running. */
const POLL_MS = 3_000;
/** Back-off after a failed settlement attempt (usually the RPC or a busy tab). */
const RETRY_MS = 6_000;

export type AutoSettleState =
  | { readonly status: "idle" }
  /** The order's batch is still collecting, aggregating, or publishing. */
  | { readonly status: "waiting"; readonly auctionId: bigint }
  | { readonly status: "settling"; readonly auctionId: bigint }
  | {
      readonly status: "retrying";
      readonly auctionId: bigint;
      readonly message: string;
    };

/**
 * Settle the trader's order as soon as its batch is final: claim fills and
 * unused reserves into the BeatEx balance, or refund a cancelled batch. The
 * demo wallet signs without prompts, so this needs no click. Also resolves
 * orders left open by an earlier visit.
 */
export function useAutoSettle(
  market: MarketConfig,
  privateState: PrivateStateView,
) {
  const { client, tracker } = useBeatEx();
  const action = useTrackedAction();
  const [state, setState] = useState<AutoSettleState>({ status: "idle" });
  // Newest first; each stays until the trader dismisses it.
  const [outcomes, setOutcomes] = useState<readonly SettlementOutcome[]>([]);
  const [attempt, setAttempt] = useState(0);

  const active =
    privateState.status === "ready"
      ? privateState.record.state.activeOrder
      : null;
  const auctionId = active?.auctionId ?? null;
  const baseReserve = active?.baseReserve ?? 0n;
  const quoteReserve = active?.quoteReserve ?? 0n;

  useEffect(() => {
    if (client === null || auctionId === null) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const later = (ms: number) => {
      timer = setTimeout(() => setAttempt((value) => value + 1), ms);
    };

    void (async () => {
      const auction = await client
        .getAuction(market.address, auctionId)
        .catch(() => null);
      if (cancelled) return;
      const settled = auction?.state === AuctionState.Settled;
      const refunded = auction?.state === AuctionState.Cancelled;
      // Another action in this tab (e.g. a deposit) goes first.
      if ((!settled && !refunded) || tracker.busy) {
        setState({ status: "waiting", auctionId });
        later(POLL_MS);
        return;
      }
      setState({ status: "settling", auctionId });
      try {
        const result = await action.run((proofOptions) =>
          refunded
            ? client.refundToBalance({
                market: market.address,
                auctionId,
                proofOptions,
              })
            : client.settleToBalance({
                market: market.address,
                auctionId,
                proofOptions,
              }),
        );
        // Record the outcome even if the settled state already re-rendered us.
        const outcome = describeSettlement(
          auctionId,
          {
            baseReserve,
            quoteReserve,
            basePayout: result.basePayout ?? baseReserve,
            quotePayout: result.quotePayout ?? quoteReserve,
          },
          refunded,
          market,
        );
        setOutcomes((current) => [
          outcome,
          ...current.filter((item) => item.auctionId !== outcome.auctionId),
        ]);
        setState({ status: "idle" });
      } catch (error) {
        if (cancelled) return;
        setState({
          status: "retrying",
          auctionId,
          message:
            error instanceof Error ? error.message : "Settlement failed.",
        });
        later(RETRY_MS);
      }
    })();

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
    // `action` is stable per mount; reserves travel with the auction id.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, market, auctionId, attempt, tracker]);

  const dismiss = useCallback(
    (auctionId: bigint) =>
      setOutcomes((current) =>
        current.filter((item) => item.auctionId !== auctionId),
      ),
    [],
  );

  return {
    state: auctionId === null ? ({ status: "idle" } as const) : state,
    progress: action.progress,
    outcomes,
    dismiss,
  };
}
