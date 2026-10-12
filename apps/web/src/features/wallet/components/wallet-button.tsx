"use client";

import { useEffect, useRef, useState } from "react";

import { publicEnv } from "@/lib/config/public-env";
import { useBeatEx } from "@/providers/beatex-provider";

function shorten(value: string) {
  return `${value.slice(0, 4)}…${value.slice(-4)}`;
}

/** The built-in Devnet demo wallet: its address, plus copy, explorer, and reset. */
export function WalletButton() {
  const { owner, walletError, resetWallet } = useBeatEx();
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetting, setResetting] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) {
        setOpen(false);
        setConfirmReset(false);
      }
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  if (walletError !== null) {
    return (
      <span title={walletError} className="text-warn text-sm">
        Wallet unavailable
      </span>
    );
  }

  const copy = async () => {
    if (owner === null) return;
    await navigator.clipboard.writeText(owner);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1_500);
  };
  const reset = async () => {
    setResetting(true);
    try {
      await resetWallet();
    } finally {
      setResetting(false);
      setConfirmReset(false);
      setOpen(false);
    }
  };
  const explorer =
    owner === null
      ? null
      : `https://explorer.solana.com/address/${owner}?cluster=${publicEnv.cluster}`;

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={owner === null}
        onClick={() => setOpen((value) => !value)}
        className="border-line-strong bg-panel flex min-h-10 items-center gap-2 rounded-lg border px-3 text-sm"
      >
        <span aria-hidden className="bg-accent size-2 rounded-full" />
        <span className="font-mono">
          {owner === null ? "Creating wallet…" : shorten(owner)}
        </span>
      </button>
      {open && owner !== null ? (
        <div
          role="menu"
          className="border-line-strong bg-panel absolute top-12 right-0 z-30 w-72 rounded-xl border p-1.5 shadow-2xl shadow-black/50"
        >
          <div className="px-3 pt-2 pb-2.5">
            <p className="text-sm font-semibold">Demo wallet</p>
            <p className="text-muted mt-0.5 text-xs leading-relaxed">
              {publicEnv.cluster === "devnet" ? "Devnet" : publicEnv.cluster} ·
              stored in this browser. Clearing site data deletes it.
            </p>
            <p className="text-faint mt-2 font-mono text-xs break-all">
              {owner}
            </p>
          </div>
          <button
            type="button"
            role="menuitem"
            onClick={() => void copy()}
            className="hover:bg-raised w-full rounded-lg px-3 py-2.5 text-left text-sm"
          >
            {copied ? "Copied" : "Copy address"}
          </button>
          {explorer !== null ? (
            <a
              role="menuitem"
              href={explorer}
              target="_blank"
              rel="noreferrer"
              className="hover:bg-raised block w-full rounded-lg px-3 py-2.5 text-left text-sm"
            >
              View on Solana Explorer
            </a>
          ) : null}
          {confirmReset ? (
            <div className="border-line mt-1 border-t px-3 pt-2.5 pb-1.5">
              <p className="text-muted text-xs leading-relaxed">
                A new wallet replaces this one. Its test funds and private
                balance stay with the old address and can&apos;t be recovered.
              </p>
              <div className="mt-2.5 flex gap-2">
                <button
                  type="button"
                  disabled={resetting}
                  onClick={() => void reset()}
                  className="bg-sell/90 text-bg min-h-8 flex-1 rounded-md text-sm font-semibold disabled:opacity-60"
                >
                  {resetting ? "Resetting…" : "Reset wallet"}
                </button>
                <button
                  type="button"
                  onClick={() => setConfirmReset(false)}
                  className="border-line-strong min-h-8 flex-1 rounded-md border text-sm"
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : (
            <button
              type="button"
              role="menuitem"
              onClick={() => setConfirmReset(true)}
              className="text-sell hover:bg-raised w-full rounded-lg px-3 py-2.5 text-left text-sm"
            >
              Reset wallet…
            </button>
          )}
        </div>
      ) : null}
    </div>
  );
}
