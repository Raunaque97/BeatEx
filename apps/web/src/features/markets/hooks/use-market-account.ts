"use client";

import { decodeMarket, type Address } from "@beatex/sdk";
import { useQuery } from "@tanstack/react-query";

import { beatexRpc } from "@/lib/beatex/rpc";
import { queryKeys } from "@/lib/query/keys";

export function useMarketAccount(market: string) {
  return useQuery({
    queryKey: queryKeys.market(market),
    queryFn: async () => {
      const account = await beatexRpc.getAccount(market as Address);
      if (account === null)
        throw new Error("Market account not found on this cluster.");
      return decodeMarket(account.data);
    },
    // Last-clearing fields change once per settled batch.
    refetchInterval: 15_000,
  });
}
