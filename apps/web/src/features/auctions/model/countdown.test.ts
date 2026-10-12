import { describe, expect, it } from "vitest";

import { INITIAL_COUNTDOWN, advanceCountdown } from "./countdown";
import type { SlotTiming } from "./slot-timing";

const schedule = { startSlot: 1_000, durationSlots: 20 };
const LEAD = 16;

function run(
  timings: (now: number) => SlotTiming,
  from: number,
  to: number,
  step = 16,
) {
  let state = INITIAL_COUNTDOWN;
  const frames = [];
  for (let now = from; now <= to; now += step) {
    const frame = advanceCountdown(
      state,
      schedule,
      timings(now),
      now,
      step,
      LEAD,
    )!;
    state = frame.state;
    frames.push({ now, ...frame });
  }
  return frames;
}

describe("advanceCountdown", () => {
  it("counts down smoothly at the measured slot rate", () => {
    const timing: SlotTiming = { slot: 1_000n, observedAt: 0, slotMs: 250 };
    const frames = run(() => timing, 0, 2_000);
    expect(frames[0]!.msLeft).toBeCloseTo(5_000, 0);
    expect(frames.at(-1)!.msLeft).toBeCloseTo(3_000, -1);
    // Every frame moves down by about one frame's worth of time.
    for (let index = 1; index < frames.length; index += 1) {
      const step = frames[index - 1]!.msLeft - frames[index]!.msLeft;
      expect(step).toBeGreaterThanOrEqual(0);
      expect(step).toBeLessThan(40);
    }
  });

  it("eases a correction in instead of jumping, and never counts up", () => {
    const early: SlotTiming = { slot: 1_000n, observedAt: 0, slotMs: 250 };
    // At t=1000 the RPC says the chain is 2 slots behind the estimate.
    const late: SlotTiming = { slot: 1_002n, observedAt: 1_000, slotMs: 250 };
    const frames = run((now) => (now < 1_000 ? early : late), 0, 2_500);
    for (let index = 1; index < frames.length; index += 1) {
      const step = frames[index - 1]!.msLeft - frames[index]!.msLeft;
      expect(step).toBeGreaterThanOrEqual(0);
      expect(step).toBeLessThan(120);
    }
    // After the easing period it agrees with the corrected clock.
    const settled = frames.find((frame) => frame.now >= 2_496)!;
    const corrected = (20 - 2 - 1_496 / 250) * 250;
    expect(Math.abs(settled.msLeft - corrected)).toBeLessThan(25);
  });

  it("resets cleanly when the next batch starts", () => {
    // Observed 4 slots before the boundary, well inside the look-ahead limit.
    const timing: SlotTiming = { slot: 1_016n, observedAt: 4_000, slotMs: 250 };
    const frames = run(() => timing, 4_900, 5_100);
    const next = frames.find((frame) => frame.batch === 1)!;
    expect(next.msLeft).toBeGreaterThan(4_800);
    expect(next.elapsed).toBeLessThan(0.05);
  });

  it("waits until the market has started", () => {
    const timing: SlotTiming = { slot: 900n, observedAt: 0, slotMs: 250 };
    expect(
      advanceCountdown(INITIAL_COUNTDOWN, schedule, timing, 0, 16, LEAD),
    ).toBeNull();
  });
});
