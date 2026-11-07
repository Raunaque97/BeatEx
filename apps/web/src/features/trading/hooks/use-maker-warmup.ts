"use client";

import { useEffect, useRef } from "react";

import { requestMaker } from "@/lib/beatex/maker";

/** Stop quoting for a trader who has walked away from the page. */
const IDLE_AFTER_MS = 60_000;
const ACTIVITY_EVENTS = ["pointermove", "pointerdown", "keydown"] as const;

/**
 * A maker takes longer to prepare than one auction lasts, so it has to start
 * before the trader clicks. Ask for one per auction while `wanted`, as long as
 * someone is here: `attended` drops the check for recent activity.
 */
export function useMakerWarmup(
  market: string,
  owner: string | null,
  auctionId: bigint | null,
  wanted: boolean,
  attended: boolean,
) {
  const lastActivity = useRef(0);
  const asked = useRef<bigint | null>(null);

  useEffect(() => {
    const note = () => {
      lastActivity.current = Date.now();
    };
    note();
    for (const event of ACTIVITY_EVENTS) {
      window.addEventListener(event, note, { passive: true });
    }
    return () => {
      for (const event of ACTIVITY_EVENTS) {
        window.removeEventListener(event, note);
      }
    };
  }, []);

  useEffect(() => {
    if (!wanted || owner === null || auctionId === null) return;
    if (document.visibilityState !== "visible") return;
    if (!attended && Date.now() - lastActivity.current > IDLE_AFTER_MS) return;
    if (asked.current === auctionId) return;
    asked.current = auctionId;
    void requestMaker(market, owner);
  }, [market, owner, auctionId, wanted, attended]);
}
