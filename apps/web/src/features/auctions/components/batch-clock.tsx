"use client";

import type { MarketAccount } from "@beatex/sdk";
import { useEffect, useRef } from "react";

import { maxLeadSlots } from "../hooks/use-slot-clock";
import type { CurrentBatch } from "../model/current-batch";
import { INITIAL_COUNTDOWN, advanceCountdown } from "../model/countdown";
import type { SlotTiming } from "../model/slot-timing";

const PHASE_LABEL: Record<CurrentBatch["phase"], string | null> = {
  "not-started": null,
  collecting: "collecting",
  aggregating: "sealing",
  publishing: "clearing",
  settled: "settled",
  cancelled: "cancelled",
  expired: "expired",
};

type Schedule = Pick<
  MarketAccount,
  "marketStartSlot" | "submissionDurationSlots"
>;

/**
 * Seconds left in the collecting batch, animated every frame (see `advanceCountdown`).
 * Writes go straight to the DOM so the rest of the page doesn't re-render per frame.
 */
export function BatchClock({
  schedule,
  timing,
  phase,
}: {
  readonly schedule: Schedule | undefined;
  readonly timing: SlotTiming | null;
  readonly phase: CurrentBatch["phase"] | null;
}) {
  const batchText = useRef<HTMLSpanElement>(null);
  const timeText = useRef<HTMLSpanElement>(null);
  const bar = useRef<HTMLDivElement>(null);
  const progress = useRef<HTMLDivElement>(null);
  const inputs = useRef({ schedule, timing });

  useEffect(() => {
    inputs.current = { schedule, timing };
  }, [schedule, timing]);

  useEffect(() => {
    let frame = 0;
    let state = INITIAL_COUNTDOWN;
    let lastFrame = performance.now();

    const draw = () => {
      frame = requestAnimationFrame(draw);
      const frameTime = performance.now();
      const frameMs = frameTime - lastFrame;
      lastFrame = frameTime;
      const { schedule, timing } = inputs.current;
      if (schedule === undefined || timing === null) return;

      const next = advanceCountdown(
        state,
        {
          startSlot: Number(schedule.marketStartSlot),
          durationSlots: Number(schedule.submissionDurationSlots),
        },
        timing,
        Date.now(),
        frameMs,
        maxLeadSlots(timing),
      );
      if (next === null) return;
      state = next.state;
      if (batchText.current) batchText.current.textContent = `#${next.batch}`;
      if (timeText.current) {
        timeText.current.textContent = `${(next.msLeft / 1000).toFixed(2)}s`;
      }
      if (bar.current) bar.current.style.width = `${next.elapsed * 100}%`;
      progress.current?.setAttribute(
        "aria-valuenow",
        String(Math.round(next.elapsed * 100)),
      );
    };
    frame = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(frame);
  }, []);

  if (schedule === undefined || timing === null) {
    return (
      <div className="text-muted min-w-40 text-xs">Syncing with Solana…</div>
    );
  }
  const label = phase === null ? null : PHASE_LABEL[phase];

  return (
    <div className="min-w-44" aria-live="off">
      <div className="text-muted flex justify-between gap-3 text-xs">
        <span>
          Auction <span ref={batchText} className="text-text font-mono" />
          {label === null ? null : ` · ${label}`}
        </span>
        <span ref={timeText} className="font-mono tabular-nums" />
      </div>
      <div
        ref={progress}
        role="progressbar"
        aria-label="Time elapsed in this auction"
        aria-valuemin={0}
        aria-valuemax={100}
        className="bg-line mt-2 h-1 rounded-full"
      >
        <div
          ref={bar}
          className="bg-accent h-1 rounded-full"
          style={{ width: "0%" }}
        />
      </div>
    </div>
  );
}
