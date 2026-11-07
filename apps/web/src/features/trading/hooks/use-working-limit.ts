"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import type { CurrentBatch } from "@/features/auctions/model/current-batch";
import type { PrivateStateView } from "@/features/balances/hooks/use-private-state";
import type { MarketConfig } from "@/lib/config/markets";
import { useBeatEx, useTrackedAction } from "@/providers/beatex-provider";

import type { SettlementOutcome } from "../model/settlement-outcome";
import { isAcceptingOrders, submitOrder } from "../model/submit-order";
import { remainingOrder, type WorkingLimit } from "../model/working-limit";

/** Give up after this many auctions in a row refuse the order. */
const MAX_FAILURES = 3;

/**
 * Keep a limit order going until it has fully filled: each time it settles short,
 * submit what is left to the next open auction at the same price. This lives in
 * the tab, so closing the tab drops whatever has not been submitted yet.
 */
export function useWorkingLimit(
  market: MarketConfig,
  batch: CurrentBatch | null,
  privateState: PrivateStateView,
  maxOrderQuantity: bigint | undefined,
) {
  const { client, tracker } = useBeatEx();
  const action = useTrackedAction();
  const [working, setWorking] = useState<WorkingLimit | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Read by the settlement that outlives any one render.
  const latest = useRef(working);
  const placing = useRef(false);
  const lastAttempt = useRef<bigint | null>(null);
  const failures = useRef(0);

  const update = useCallback((next: WorkingLimit | null) => {
    latest.current = next;
    setWorking(next);
  }, []);

  const start = useCallback(
    (next: WorkingLimit) => {
      failures.current = 0;
      setError(null);
      update(next);
    },
    [update],
  );
  const stop = useCallback(() => update(null), [update]);

  /** Count a settled order. Returns whether its outcome is worth showing. */
  const onSettled = useCallback(
    (outcome: SettlementOutcome) => {
      const current = latest.current;
      if (current === null || current.auctionId !== outcome.auctionId) {
        return true;
      }
      const filled = current.filled + outcome.baseFilled;
      update(
        filled >= current.total
          ? null
          : { ...current, filled, auctionId: null },
      );
      // The status line already says it is still waiting for a match.
      return outcome.baseFilled > 0n;
    },
    [update],
  );

  const settled =
    privateState.status === "ready" &&
    privateState.record.state.activeOrder === null;

  useEffect(() => {
    if (
      working === null ||
      working.auctionId !== null ||
      client === null ||
      batch === null ||
      batch.grid === null ||
      maxOrderQuantity === undefined ||
      !settled ||
      !isAcceptingOrders(batch) ||
      tracker.busy ||
      placing.current ||
      lastAttempt.current === batch.auctionId
    ) {
      return;
    }
    const built = remainingOrder(working, batch.grid, maxOrderQuantity, market);
    if (built === null) return;
    placing.current = true;
    lastAttempt.current = batch.auctionId;
    void (async () => {
      try {
        if (!built.ok) {
          failures.current = MAX_FAILURES;
          throw new Error(built.error);
        }
        const auctionId = await action.run((proofOptions) =>
          submitOrder({
            client,
            market,
            order: built.order,
            batch,
            replacing: null,
            proofOptions,
          }),
        );
        failures.current = 0;
        if (latest.current !== null) update({ ...latest.current, auctionId });
      } catch (cause) {
        failures.current += 1;
        if (failures.current >= MAX_FAILURES) {
          setError(
            cause instanceof Error ? cause.message : "The order was refused.",
          );
          update(null);
        }
      } finally {
        placing.current = false;
      }
    })();
    // `action` is stable per mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [working, client, batch, maxOrderQuantity, settled, tracker, market]);

  return { working, error, progress: action.progress, start, stop, onSettled };
}

export type WorkingLimitState = ReturnType<typeof useWorkingLimit>;
