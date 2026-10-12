"use client";

import { decodeMarket, type Address, type MarketAccount } from "@beatex/sdk";
import { useQueries } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import { beatexRpc } from "@/lib/beatex/rpc";
import { MARKETS, type MarketConfig } from "@/lib/config/markets";
import { fundsByToken, type MarketFunds } from "@/lib/funds";
import { queryKeys } from "@/lib/query/keys";
import { useBeatEx } from "@/providers/beatex-provider";

import { loadPrivateState, type PrivateStateView } from "./use-private-state";

const RETRY_MS = 5_000;

/**
 * Private balances of every market except the current one. Read-only: the
 * current market's hook owns recovery. Kept in component state, never in
 * React Query, like all private data.
 */
function useOtherPrivateStates(current: string) {
  const { client, privateRevision } = useBeatEx();
  const [views, setViews] = useState<{
    readonly client: unknown;
    readonly byMarket: ReadonlyMap<string, PrivateStateView>;
  } | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (client === null) return;
    let cancelled = false;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const others = MARKETS.filter((market) => market.address !== current);
    void Promise.all(
      others.map(
        async (market) =>
          [
            market.address,
            await loadPrivateState(client, market.address, false),
          ] as const,
      ),
    ).then((entries) => {
      if (cancelled) return;
      setViews({ client, byMarket: new Map(entries) });
      if (entries.some(([, view]) => view.status === "error")) {
        retry = setTimeout(() => setAttempt((value) => value + 1), RETRY_MS);
      }
    });
    return () => {
      cancelled = true;
      clearTimeout(retry);
    };
  }, [client, current, privateRevision, attempt]);

  return views !== null && views.client === client ? views.byMarket : null;
}

function marketFunds(
  market: MarketConfig,
  account: MarketAccount,
  view: PrivateStateView,
  price: bigint | null,
): MarketFunds | null {
  if (view.status !== "ready" && view.status !== "none") return null;
  const state = view.status === "ready" ? view.record.state : null;
  const active = state?.activeOrder ?? null;
  return {
    market,
    baseMint: account.baseMint,
    quoteMint: account.quoteMint,
    freeBase: state?.balance.baseValue ?? 0n,
    freeQuote: state?.balance.quoteValue ?? 0n,
    lockedBase: active?.baseReserve ?? 0n,
    lockedQuote: active?.quoteReserve ?? 0n,
    price,
  };
}

/**
 * The trader's funds across every market, so the app can show one balance per
 * token and move funds to wherever an order needs them.
 */
export function useMarketFunds(
  current: MarketConfig,
  currentView: PrivateStateView,
  /** The current market's display price (last clear, or Pyth before any). */
  currentPrice: bigint | null,
) {
  const accounts = useQueries({
    queries: MARKETS.map((market) => ({
      queryKey: queryKeys.market(market.address),
      queryFn: async () => {
        const account = await beatexRpc.getAccount(market.address as Address);
        if (account === null)
          throw new Error("Market account not found on this cluster.");
        return decodeMarket(account.data);
      },
      refetchInterval: 15_000,
    })),
  });
  const others = useOtherPrivateStates(current.address);

  const markets: MarketFunds[] = [];
  let complete = true;
  MARKETS.forEach((market, index) => {
    const account = accounts[index]?.data;
    const view =
      market.address === current.address
        ? currentView
        : (others?.get(market.address) ?? null);
    if (account === undefined || view === null) {
      complete = false;
      return;
    }
    const lastClear =
      account.lastClearingPrice > 0n ? account.lastClearingPrice : null;
    const funds = marketFunds(
      market,
      account,
      view,
      market.address === current.address
        ? (currentPrice ?? lastClear)
        : lastClear,
    );
    if (funds === null) complete = false;
    else markets.push(funds);
  });

  return { markets, tokens: fundsByToken(markets), complete };
}
