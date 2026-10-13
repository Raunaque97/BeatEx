const BEATS = 6;
const BEAT_W = 200;

/** One flat stretch and one spike per batch, like the BeatEx mark. */
const BEAT_PATH =
  "M0 70" +
  " h70 l10 -8 l10 8 h12 l8 -48 l10 86 l8 -50 l10 12 h62".repeat(BEATS);

/**
 * The hero's heartbeat line: a faint trace with a bright pulse running along
 * it. Each spike stands for a batch clearing.
 */
export function Heartbeat() {
  return (
    <svg
      viewBox={`0 0 ${BEATS * BEAT_W} 120`}
      preserveAspectRatio="none"
      className="block h-24 w-full sm:h-28"
      aria-hidden
    >
      <path
        d={BEAT_PATH}
        fill="none"
        className="stroke-line-strong"
        strokeWidth={2}
        strokeLinejoin="round"
        vectorEffect="non-scaling-stroke"
      />
      <path
        d={BEAT_PATH}
        pathLength={1}
        fill="none"
        className="stroke-accent hero-pulse"
        strokeWidth={3}
        strokeLinecap="round"
        strokeLinejoin="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
