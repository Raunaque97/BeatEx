"use client";

import { useQuery } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";

import { beatexRpc } from "@/lib/beatex/rpc";
import { queryKeys } from "@/lib/query/keys";

import {
  estimateSlot,
  observeSlot,
  type SlotTiming,
} from "../model/slot-timing";

/** How often the RPC is asked for the real slot; each answer realigns the clock. */
const POLL_MS = 2_000;
/** Re-render cadence for slot-dependent views; the header clock animates per frame. */
const TICK_MS = 250;
/** Never project more than two poll intervals past the last observation. */
const MAX_LEAD_MS = 2 * POLL_MS;

/**
 * The current slot: polled from RPC every two seconds and projected locally between
 * polls using a running average of the measured slot time. Display-only; every
 * protocol decision uses on-chain slots.
 */
export function useSlotClock(): {
  readonly slot: bigint | null;
  readonly timing: SlotTiming | null;
} {
  const timing = useRef<SlotTiming | null>(null);
  const { data } = useQuery({
    queryKey: queryKeys.slot(),
    queryFn: async () => {
      const requested = Date.now();
      const slot = await beatexRpc.getSlot();
      // Assume the RPC read the slot halfway through the round trip.
      const observedAt = (requested + Date.now()) / 2;
      timing.current = observeSlot(timing.current, slot, observedAt);
      return timing.current;
    },
    refetchInterval: POLL_MS,
    staleTime: 0,
  });
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), TICK_MS);
    return () => clearInterval(timer);
  }, []);

  if (data === undefined) return { slot: null, timing: null };
  return {
    slot: estimateSlot(data, now, maxLeadSlots(data)),
    timing: data,
  };
}

/** How many slots past the last observation an estimate may run. */
export function maxLeadSlots(timing: SlotTiming): number {
  return Math.ceil(MAX_LEAD_MS / timing.slotMs);
}
