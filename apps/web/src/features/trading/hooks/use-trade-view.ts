"use client";

import { useCallback, useSyncExternalStore } from "react";

export type TradeView = "simple" | "advanced";

const KEY = "beatex:trade-view";
const CHANGE = "beatex:trade-view-change";
/** Used when storage is unavailable (private mode, blocked site data). */
let memory: TradeView = "simple";

function read(): TradeView {
  try {
    const stored = window.localStorage.getItem(KEY);
    if (stored === "simple" || stored === "advanced") return stored;
  } catch {
    // Fall back to this page's memory.
  }
  return memory;
}

function subscribe(onChange: () => void) {
  window.addEventListener(CHANGE, onChange);
  // Another tab changed it.
  window.addEventListener("storage", onChange);
  return () => {
    window.removeEventListener(CHANGE, onChange);
    window.removeEventListener("storage", onChange);
  };
}

/**
 * The Simple / Advanced choice, remembered in this browser so it survives
 * switching markets (a new page) and reloads.
 */
export function useTradeView() {
  const view = useSyncExternalStore(subscribe, read, () => "simple" as const);
  const setView = useCallback((next: TradeView) => {
    memory = next;
    try {
      window.localStorage.setItem(KEY, next);
    } catch {
      // Memory still holds it for this session.
    }
    window.dispatchEvent(new Event(CHANGE));
  }, []);
  return [view, setView] as const;
}
