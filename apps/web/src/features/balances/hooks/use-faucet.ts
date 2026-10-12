"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";

import { queryKeys } from "@/lib/query/keys";
import { useBeatEx } from "@/providers/beatex-provider";

type FaucetStatus = "idle" | "sending" | "sent" | "failed";

export function useFaucet() {
  const { owner } = useBeatEx();
  const queryClient = useQueryClient();
  const [status, setStatus] = useState<FaucetStatus>("idle");
  const [message, setMessage] = useState<string | null>(null);

  /** Resolves true once the tokens are sent. */
  const request = useCallback(async (): Promise<boolean> => {
    if (owner === null) return false;
    setStatus("sending");
    setMessage(null);
    try {
      const response = await fetch("/api/faucet", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ owner }),
      });
      const body = (await response.json()) as { error?: string };
      if (!response.ok)
        throw new Error(body.error ?? "The faucet is unavailable.");
      setStatus("sent");
      setMessage("Test funds sent to your wallet.");
      await queryClient.invalidateQueries({
        queryKey: queryKeys.walletBalances(owner),
      });
      return true;
    } catch (error) {
      setStatus("failed");
      setMessage(
        error instanceof Error ? error.message : "The faucet is unavailable.",
      );
      return false;
    }
  }, [owner, queryClient]);

  return { status, message, request };
}
