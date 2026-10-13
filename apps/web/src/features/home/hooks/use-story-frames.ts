"use client";

import { useEffect, useRef, useState, useSyncExternalStore } from "react";

/** The last frame lingers so its caption can be read before the story loops. */
const LAST_FRAME_HOLD = 1.6;

const REDUCED_MOTION = "(prefers-reduced-motion: reduce)";

function subscribeReducedMotion(onChange: () => void) {
  const query = window.matchMedia(REDUCED_MOTION);
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

function prefersReducedMotion() {
  return window.matchMedia(REDUCED_MOTION).matches;
}

/**
 * Drives one storyboard panel: advances through `count` frames every `frameMs`
 * while the panel is on screen and playing. `tick` counts every advance (it
 * never wraps) for diagrams that move continuously, like the batch pipeline;
 * those pass `holdLast: false` so every step takes the same time.
 * With reduced motion the story only moves when the reader steps it.
 */
export function useStoryFrames(
  count: number,
  frameMs: number,
  { holdLast = true }: { readonly holdLast?: boolean } = {},
) {
  const ref = useRef<HTMLElement>(null);
  const [tick, setTick] = useState(0);
  const [visible, setVisible] = useState(false);
  const [playing, setPlaying] = useState(true);
  const reducedMotion = useSyncExternalStore(
    subscribeReducedMotion,
    prefersReducedMotion,
    () => false,
  );
  const frame = tick % count;

  useEffect(() => {
    const element = ref.current;
    if (element === null) return;
    const observer = new IntersectionObserver(
      ([entry]) => setVisible(entry?.isIntersecting ?? false),
      { threshold: 0.35 },
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!visible || !playing || reducedMotion) return;
    const delay =
      holdLast && frame === count - 1 ? frameMs * LAST_FRAME_HOLD : frameMs;
    const timer = window.setTimeout(() => setTick((t) => t + 1), delay);
    return () => window.clearTimeout(timer);
  }, [visible, playing, reducedMotion, tick, frame, count, frameMs, holdLast]);

  return {
    ref,
    frame,
    tick,
    playing: playing && !reducedMotion,
    togglePlaying: () => setPlaying((p) => !p),
    /** Jump forward to `target`, so `tick` keeps increasing. */
    goTo: (target: number) =>
      setTick(
        (t) => t - (t % count) + target + (target < t % count ? count : 0),
      ),
  };
}
