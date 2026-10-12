"use client";

import { useState } from "react";

import { SOL_TOP_UP_FLOOR_LAMPORTS } from "@/lib/config/faucet";
import type { MarketConfig } from "@/lib/config/markets";
import { useBeatEx } from "@/providers/beatex-provider";

import { useFaucet } from "../hooks/use-faucet";
import type { PrivateStateView } from "../hooks/use-private-state";
import { useWalletBalances } from "../hooks/use-wallet-balances";

/** How long the "sent" confirmation stays before the banner lifts away. */
const THANKS_MS = 1_800;
/** Matches the `banner-close` animation in globals.css. */
const EXIT_MS = 320;

/**
 * Slides in above the header when the demo wallet holds no test tokens or is low
 * on SOL for fees, and requests them from the faucet in one click. Key it by owner so a wallet switch
 * shows it again.
 */
export function FaucetBanner({
  market,
  privateState,
}: {
  readonly market: MarketConfig;
  readonly privateState: PrivateStateView;
}) {
  const { owner } = useBeatEx();
  const wallet = useWalletBalances(owner);
  const faucet = useFaucet();
  const [leaving, setLeaving] = useState(false);
  const [closed, setClosed] = useState(false);

  const balance =
    privateState.status === "ready" ? privateState.record.state.balance : null;
  const hasPrivateFunds =
    balance !== null && (balance.baseValue > 0n || balance.quoteValue > 0n);
  const needsTokens =
    wallet.data !== undefined &&
    wallet.data.baseAtoms === 0n &&
    wallet.data.quoteAtoms === 0n &&
    !hasPrivateFunds;
  const lowOnSol =
    wallet.data !== undefined &&
    wallet.data.lamports < SOL_TOP_UP_FLOOR_LAMPORTS;
  const needsFunds = owner !== null && (needsTokens || lowOnSol);
  // Once sent, the refreshed balance no longer qualifies; stay up to say so.
  if (closed || !(needsFunds || faucet.status === "sent")) return null;

  const dismiss = () => {
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    setLeaving(true);
    // A timer rather than animationend, which hidden tabs hold back.
    window.setTimeout(() => setClosed(true), reduced ? 0 : EXIT_MS);
  };
  const request = async () => {
    if (await faucet.request()) window.setTimeout(dismiss, THANKS_MS);
  };

  const sent = faucet.status === "sent";
  const sending = faucet.status === "sending";
  const tokens = `${market.base.symbol}, ${market.quote.symbol}`;

  return (
    <div className="faucet-banner" data-leaving={leaving ? "" : undefined}>
      <div
        role="status"
        className="faucet-banner-bar border-accent/30 bg-panel border-b"
      >
        <div className="flex flex-wrap items-center justify-center gap-x-4 gap-y-2 px-5 py-2.5 text-sm">
          <span
            aria-hidden
            className={`size-2 rounded-full ${sent ? "bg-accent" : "bg-accent animate-pulse"}`}
          />
          <p className="text-center">
            {sent ? (
              <>
                <span className="font-semibold">Test funds on the way.</span>{" "}
                <span className="text-muted">
                  Deposit them to start trading.
                </span>
              </>
            ) : faucet.status === "failed" ? (
              <span className="text-warn">{faucet.message}</span>
            ) : (
              <>
                <span className="font-semibold">
                  {needsTokens
                    ? "Your demo wallet is empty."
                    : "Your demo wallet is low on SOL."}
                </span>{" "}
                <span className="text-muted">
                  {needsTokens
                    ? `Grab free Devnet ${tokens} and SOL to try BeatEx.`
                    : "Top up to keep paying fees and order rent."}
                </span>
              </>
            )}
          </p>
          {sent ? null : (
            <button
              type="button"
              disabled={sending}
              onClick={() => void request()}
              className="faucet-banner-cta bg-accent text-accent-ink min-h-8 rounded-md px-3.5 text-sm font-semibold disabled:opacity-60"
            >
              {sending
                ? "Sending…"
                : faucet.status === "failed"
                  ? "Try again"
                  : "Get test funds"}
            </button>
          )}
          <button
            type="button"
            aria-label="Dismiss"
            onClick={dismiss}
            className="text-faint hover:text-text -mr-2 grid size-8 place-items-center rounded-md"
          >
            <svg aria-hidden viewBox="0 0 16 16" className="size-3.5">
              <path
                d="M4 4l8 8M12 4l-8 8"
                stroke="currentColor"
                strokeWidth="1.6"
                strokeLinecap="round"
              />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
