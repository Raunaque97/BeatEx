/** Solana's target slot time; the starting point before any rate is measured. */
export const TARGET_SLOT_MS = 250;

/** Weight of each new measurement in the running average. */
const SMOOTHING = 0.2;
/** Plausible bounds for one measured slot; anything outside is a hiccup, not a rate. */
const MIN_SLOT_MS = 100;
const MAX_SLOT_MS = 1_000;
/** Measurements over shorter windows are dominated by RPC latency jitter. */
const MIN_SAMPLE_WINDOW_MS = 1_000;

export interface SlotTiming {
  /** Last slot reported by the RPC. */
  readonly slot: bigint;
  /** Local time the slot was observed (midpoint of the request). */
  readonly observedAt: number;
  /** Running average slot duration in milliseconds. */
  readonly slotMs: number;
}

/**
 * Realign to a fresh RPC observation and fold the slot rate measured since the
 * previous observation into the running average.
 */
export function observeSlot(
  previous: SlotTiming | null,
  slot: bigint,
  observedAt: number,
): SlotTiming {
  if (previous === null) return { slot, observedAt, slotMs: TARGET_SLOT_MS };
  const elapsed = observedAt - previous.observedAt;
  const advanced = Number(slot - previous.slot);
  let slotMs = previous.slotMs;
  if (elapsed >= MIN_SAMPLE_WINDOW_MS && advanced > 0) {
    const measured = elapsed / advanced;
    if (measured >= MIN_SLOT_MS && measured <= MAX_SLOT_MS) {
      slotMs = previous.slotMs + SMOOTHING * (measured - previous.slotMs);
    }
  }
  return { slot, observedAt, slotMs };
}

/** The current slot projected from the last observation at the average rate. */
export function estimateSlot(
  timing: SlotTiming,
  now: number,
  maxLeadSlots: number,
): bigint {
  const lead = Math.floor(Math.max(0, now - timing.observedAt) / timing.slotMs);
  return timing.slot + BigInt(Math.min(lead, maxLeadSlots));
}

/** Fractional slot projected from the last observation, for smooth animation. */
export function estimateSlotFraction(
  timing: SlotTiming,
  now: number,
  maxLeadSlots: number,
): number {
  const lead = Math.max(0, now - timing.observedAt) / timing.slotMs;
  return Number(timing.slot) + Math.min(lead, maxLeadSlots);
}
