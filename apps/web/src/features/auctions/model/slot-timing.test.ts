import { describe, expect, it } from "vitest";

import { TARGET_SLOT_MS, estimateSlot, observeSlot } from "./slot-timing";

describe("slot timing", () => {
  it("starts at Solana's 250 ms target", () => {
    expect(observeSlot(null, 100n, 0)).toEqual({
      slot: 100n,
      observedAt: 0,
      slotMs: TARGET_SLOT_MS,
    });
  });

  it("moves the running average toward the measured rate", () => {
    let timing = observeSlot(null, 0n, 0);
    // The chain is actually producing a slot every 400 ms.
    for (let poll = 1; poll <= 20; poll += 1) {
      timing = observeSlot(timing, BigInt(poll * 5), poll * 2_000);
    }
    expect(timing.slotMs).toBeGreaterThan(395);
    expect(timing.slotMs).toBeLessThan(405);
  });

  it("realigns to every observation, including backwards corrections", () => {
    const ahead = observeSlot(null, 100n, 0);
    expect(estimateSlot(ahead, 2_000, 12)).toBe(108n);
    const corrected = observeSlot(ahead, 106n, 2_000);
    expect(estimateSlot(corrected, 2_000, 12)).toBe(106n);
  });

  it("ignores stalls, short windows, and implausible rates", () => {
    const start = observeSlot(null, 100n, 0);
    expect(observeSlot(start, 100n, 2_000).slotMs).toBe(TARGET_SLOT_MS);
    expect(observeSlot(start, 101n, 300).slotMs).toBe(TARGET_SLOT_MS);
    expect(observeSlot(start, 200n, 2_000).slotMs).toBe(TARGET_SLOT_MS);
  });

  it("caps how far the estimate runs ahead of the last observation", () => {
    const timing = observeSlot(null, 0n, 0);
    expect(estimateSlot(timing, 60_000, 12)).toBe(12n);
    expect(estimateSlot(timing, -500, 12)).toBe(0n);
  });
});
