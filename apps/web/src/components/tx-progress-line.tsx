"use client";

import type { TxProgress } from "@/lib/beatex/tx-tracker";
import { publicEnv } from "@/lib/config/public-env";

const STEPS = [
  { phase: "building", label: "Build" },
  { phase: "proving", label: "Prove" },
  { phase: "simulating", label: "Check" },
  { phase: "awaiting-signature", label: "Sign" },
  { phase: "submitted", label: "Confirm" },
] as const;

const ORDER = [
  "building",
  "proving",
  "simulating",
  "awaiting-signature",
  "submitted",
  "confirmed",
];

function explorerUrl(signature: string) {
  return `https://explorer.solana.com/tx/${signature}?cluster=${publicEnv.cluster === "localnet" ? "custom" : publicEnv.cluster}`;
}

/** One compact line of steps that replaces the action button while it works. */
export function TxProgressLine({
  title,
  progress,
}: {
  readonly title: string;
  readonly progress: TxProgress;
}) {
  const reached = ORDER.indexOf(progress.phase);
  const failed = progress.phase === "failed";
  const done = progress.phase === "confirmed";

  return (
    <div
      role="status"
      aria-live="polite"
      className={`flex flex-col gap-2.5 rounded-2xl border px-4 py-3.5 ${
        failed
          ? "border-sell/60 bg-sell-deep/40"
          : done
            ? "border-accent/60 bg-accent/5"
            : "border-buy/50 bg-buy-deep/40"
      }`}
    >
      <div className="flex items-center justify-between gap-3 text-sm">
        <strong className="font-semibold">
          {failed
            ? "Didn't go through"
            : done
              ? `${title} confirmed`
              : `${title}…`}
        </strong>
        {progress.provingMs !== undefined ? (
          <span className="text-muted font-mono text-xs">
            proof {(progress.provingMs / 1000).toFixed(1)} s
          </span>
        ) : null}
      </div>
      {failed ? (
        <p className="text-sell-soft text-xs leading-relaxed">
          {progress.error}
        </p>
      ) : (
        <ol className="flex flex-wrap gap-x-4 gap-y-1 text-xs">
          {STEPS.map((step, index) => {
            const stepIndex = ORDER.indexOf(step.phase);
            const state =
              done || reached > stepIndex
                ? "done"
                : reached === stepIndex
                  ? "now"
                  : "todo";
            return (
              <li
                key={step.phase}
                className={
                  state === "done"
                    ? "text-accent"
                    : state === "now"
                      ? "text-buy-soft"
                      : "text-faint"
                }
              >
                {state === "done" ? "✓" : `${index + 1}.`} {step.label}
                {step.phase === "awaiting-signature" &&
                state === "now" &&
                (progress.step ?? 1) > 1
                  ? ` (${progress.step})`
                  : ""}
              </li>
            );
          })}
        </ol>
      )}
      {progress.signature !== undefined ? (
        <a
          href={explorerUrl(progress.signature)}
          target="_blank"
          rel="noreferrer"
          className="text-muted hover:text-text self-start font-mono text-[11px] underline-offset-4 hover:underline"
        >
          View transaction ↗
        </a>
      ) : null}
    </div>
  );
}
