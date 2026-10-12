"use client";

import {
  bytesEqual,
  decodePythPriceUpdate,
  hasOracle,
  type Address,
  type MarketAccount,
} from "@beatex/sdk";
import { useQuery } from "@tanstack/react-query";

import { beatexRpc } from "@/lib/beatex/rpc";
import { queryKeys } from "@/lib/query/keys";

/**
 * The market's configured Pyth price account, checked against its feed ID.
 * Oracle-free markets have none, so the query stays idle and yields no data.
 */
export function useOraclePrice(
  market: string,
  marketAccount: MarketAccount | undefined,
) {
  return useQuery({
    queryKey: queryKeys.oracle(market),
    enabled: marketAccount !== undefined && hasOracle(marketAccount),
    queryFn: async () => {
      const account = await beatexRpc.getAccount(
        marketAccount!.oraclePriceAccount as Address,
      );
      if (account === null) throw new Error("Pyth price account not found.");
      const update = decodePythPriceUpdate(account.data);
      if (!bytesEqual(update.feedId, marketAccount!.oracleFeedId)) {
        throw new Error("Pyth account holds an unexpected price feed.");
      }
      return update;
    },
    refetchInterval: 15_000,
  });
}
