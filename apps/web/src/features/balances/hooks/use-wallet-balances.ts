"use client";

import { GENERATED_DEPLOYMENT } from "@beatex/sdk";
import { address } from "@solana/kit";
import { useQuery } from "@tanstack/react-query";

import { beatexRpc } from "@/lib/beatex/rpc";
import { queryKeys } from "@/lib/query/keys";
import { associatedTokenAddress, tokenAccountAmount } from "@/lib/solana/token";

export interface WalletBalances {
  readonly lamports: bigint;
  readonly baseAccount: string;
  readonly quoteAccount: string;
  /** Token atoms (8-decimal tBTC, 6-decimal tUSD). */
  readonly baseAtoms: bigint;
  readonly quoteAtoms: bigint;
}

export interface WalletMints {
  readonly base: string;
  readonly quote: string;
}

/** The faucet's tokens: the deployment's tBTC and tUSD. */
const FAUCET_MINTS: WalletMints = {
  base: GENERATED_DEPLOYMENT.baseMint,
  quote: GENERATED_DEPLOYMENT.quoteMint,
};

/**
 * Public wallet balances: SOL and the owner's Token-2022 accounts for one
 * market's tokens (the faucet's tBTC and tUSD by default).
 */
export function useWalletBalances(
  owner: string | null,
  mints: WalletMints | null = FAUCET_MINTS,
) {
  return useQuery({
    queryKey: [
      ...queryKeys.walletBalances(owner ?? ""),
      mints?.base ?? "",
      mints?.quote ?? "",
    ],
    enabled: owner !== null && mints !== null,
    refetchInterval: 15_000,
    queryFn: async (): Promise<WalletBalances> => {
      const ownerAddress = address(owner!);
      const [baseAccount, quoteAccount] = await Promise.all([
        associatedTokenAddress(ownerAddress, address(mints!.base)),
        associatedTokenAddress(ownerAddress, address(mints!.quote)),
      ]);
      const [wallet, base, quote] = await beatexRpc.getMultipleAccounts([
        ownerAddress,
        baseAccount,
        quoteAccount,
      ]);
      return {
        lamports: wallet?.lamports ?? 0n,
        baseAccount,
        quoteAccount,
        baseAtoms: base === null ? 0n : tokenAccountAmount(base.data),
        quoteAtoms: quote === null ? 0n : tokenAccountAmount(quote.data),
      };
    },
  });
}

/** The owner's public wallet amount of each mint (token atoms; 0 without an account). */
export function useWalletTokens(
  owner: string | null,
  mints: readonly string[],
) {
  const sorted = [...new Set(mints)].sort();
  return useQuery({
    queryKey: [...queryKeys.walletBalances(owner ?? ""), "tokens", ...sorted],
    enabled: owner !== null && sorted.length > 0,
    refetchInterval: 15_000,
    queryFn: async (): Promise<ReadonlyMap<string, bigint>> => {
      const ownerAddress = address(owner!);
      const accounts = await Promise.all(
        sorted.map((mint) =>
          associatedTokenAddress(ownerAddress, address(mint)),
        ),
      );
      const infos = await beatexRpc.getMultipleAccounts(accounts);
      return new Map(
        sorted.map((mint, index) => {
          const info = infos[index];
          return [
            mint,
            info === null || info === undefined
              ? 0n
              : tokenAccountAmount(info.data),
          ];
        }),
      );
    },
  });
}
