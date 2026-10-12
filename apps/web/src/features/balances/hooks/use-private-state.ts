"use client";

import {
  BeatExError,
  type BeatExClient,
  type PrivateStateRecord,
} from "@beatex/sdk";
import { useEffect, useState } from "react";

import { StateDecryptionError } from "@/lib/beatex/state-encryption";
import { useBeatEx } from "@/providers/beatex-provider";

export type PrivateStateView =
  | { readonly status: "locked" }
  | { readonly status: "loading" }
  /** No private balance on-chain or locally: the trader has never deposited. */
  | { readonly status: "none" }
  | { readonly status: "ready"; readonly record: PrivateStateRecord }
  /** A signed transaction was interrupted; it must be recovered before continuing. */
  | { readonly status: "pending" }
  /** The account exists on-chain but this browser has no (or stale) secrets for it. */
  | { readonly status: "unavailable"; readonly reason: string }
  | { readonly status: "error"; readonly message: string };

const RETRY_MS = 4_000;

/**
 * The trader's decrypted balance and order secrets. Kept in component state only;
 * never in React Query, URLs, or logs.
 */
export function usePrivateState(market: string): PrivateStateView {
  const { client, privateRevision, tracker } = useBeatEx();
  const [loaded, setLoaded] = useState<{
    readonly client: BeatExClient;
    readonly market: string;
    readonly view: PrivateStateView;
  } | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (client === null) return;
    let cancelled = false;
    let retry: ReturnType<typeof setTimeout> | undefined;
    // Resolve an interrupted transaction automatically, unless it belongs to
    // an action still running in this tab, which resolves it itself.
    void loadPrivateState(client, market, !tracker.busy).then((view) => {
      if (cancelled) return;
      if (view.status === "pending") {
        setLoaded({ client, market, view });
        retry = setTimeout(() => setAttempt((value) => value + 1), RETRY_MS);
        return;
      }
      if (view.status !== "error") {
        setLoaded({ client, market, view });
        return;
      }
      // Usually a rate-limited RPC. Keep the last good view and try again.
      retry = setTimeout(() => setAttempt((value) => value + 1), RETRY_MS);
      setLoaded((previous) =>
        previous !== null &&
        previous.client === client &&
        previous.market === market &&
        previous.view.status !== "error"
          ? previous
          : {
              client,
              market,
              view: { ...view, message: `${view.message} Retrying…` },
            },
      );
    });
    return () => {
      cancelled = true;
      clearTimeout(retry);
    };
  }, [client, market, privateRevision, attempt, tracker]);

  if (client === null) return { status: "locked" };
  // Keep showing the last result for this session while a reload is in flight.
  if (loaded !== null && loaded.client === client && loaded.market === market) {
    return loaded.view;
  }
  return { status: "loading" };
}

export async function loadPrivateState(
  client: BeatExClient,
  market: string,
  recover: boolean,
): Promise<PrivateStateView> {
  try {
    const record = await client.getPrivateState(market);
    if (record !== null) return { status: "ready", record };
    const onchain = await client.getPrivateBalance(market);
    return onchain === null
      ? { status: "none" }
      : {
          status: "unavailable",
          reason:
            "This wallet has a private balance, but its secrets aren't in this browser. Import your backup to continue.",
        };
  } catch (error) {
    if (
      error instanceof BeatExError &&
      error.code === "PRIVATE_STATE_TRANSITION_PENDING"
    ) {
      if (!recover) return { status: "pending" };
      try {
        // Commits it if it landed, drops it if it failed or expired.
        await client.recoverPendingTransition(market);
      } catch {
        // Outcome still unknown (e.g. the RPC is rate limiting); retried shortly.
        return { status: "pending" };
      }
      return loadPrivateState(client, market, false);
    }
    if (
      error instanceof BeatExError &&
      error.code === "PRIVATE_STATE_CONFLICT"
    ) {
      return {
        status: "unavailable",
        reason:
          "This browser's private state is out of date with the chain. Import your latest backup.",
      };
    }
    if (error instanceof StateDecryptionError) {
      return { status: "unavailable", reason: error.message };
    }
    return {
      status: "error",
      message:
        error instanceof Error
          ? error.message
          : "Could not load your private balance.",
    };
  }
}
