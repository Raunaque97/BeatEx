import { estimateSlotFraction, type SlotTiming } from "./slot-timing";

/** How quickly a correction from a fresh RPC observation is blended in. */
const CORRECTION_MS = 400;

export interface CountdownState {
  readonly batch: number | null;
  readonly shownMs: number;
  readonly offsetMs: number;
  readonly timing: SlotTiming | null;
}

export interface CountdownFrame {
  readonly state: CountdownState;
  readonly batch: number;
  /** Milliseconds left in the batch's collection window, as displayed. */
  readonly msLeft: number;
  /** Fraction of the window elapsed, 0..1. */
  readonly elapsed: number;
}

export const INITIAL_COUNTDOWN: CountdownState = {
  batch: null,
  shownMs: 0,
  offsetMs: 0,
  timing: null,
};

/**
 * Advance the displayed countdown by one animation frame. A fresh RPC observation
 * (new `timing`) is eased in rather than jumped to, and within a batch the
 * displayed time never increases.
 */
export function advanceCountdown(
  previous: CountdownState,
  schedule: { readonly startSlot: number; readonly durationSlots: number },
  timing: SlotTiming,
  now: number,
  frameMs: number,
  maxLeadSlots: number,
): CountdownFrame | null {
  const slot = estimateSlotFraction(timing, now, maxLeadSlots);
  if (slot < schedule.startSlot) return null;
  const relative = slot - schedule.startSlot;
  const batch = Math.floor(relative / schedule.durationSlots);
  const targetMs =
    ((batch + 1) * schedule.durationSlots - relative) * timing.slotMs;

  let offsetMs = previous.offsetMs;
  if (batch !== previous.batch) offsetMs = 0;
  else if (timing !== previous.timing) offsetMs = previous.shownMs - targetMs;
  offsetMs *= Math.exp(-frameMs / CORRECTION_MS);

  let shownMs = Math.max(0, targetMs + offsetMs);
  if (batch === previous.batch && shownMs > previous.shownMs) {
    shownMs = previous.shownMs;
  }
  const total = schedule.durationSlots * timing.slotMs;
  return {
    state: { batch, shownMs, offsetMs, timing },
    batch,
    msLeft: shownMs,
    elapsed: Math.min(1, Math.max(0, 1 - shownMs / total)),
  };
}
