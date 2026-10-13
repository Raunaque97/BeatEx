import type { DiagramProps } from "../story-chapter";
import { Stage } from "./stage";

/** Width of one collection window; every other phase is drawn relative to it. */
const WINDOW = 112;
const ROW_H = 34;
const BAR_H = 22;
/** The fixed "now" line the timeline scrolls past. */
const NOW_X = 292;
/** Where the batch that is currently collecting sits vertically. */
const NOW_ROW_Y = 166;
const CHART = { x: 16, y: 56, w: 448, h: 196 } as const;
const FIRST_BATCH = 1041;

const PHASES = [
  { key: "collect", label: "Collect", width: WINDOW, className: "fill-accent" },
  { key: "aggregate", label: "Aggregate", width: 34, className: "fill-muted" },
  {
    key: "clear",
    label: "Reveal and clear",
    width: 30,
    className: "fill-warn",
  },
  { key: "settle", label: "Settle", width: 58, className: "fill-buy" },
] as const;

type PhaseKey = (typeof PHASES)[number]["key"];

/** Each phase's start, measured from the start of its batch's collection. */
const PHASE_START = PHASES.reduce<number[]>(
  (starts, phase, index) => [
    ...starts,
    index === 0 ? 0 : starts[index - 1]! + PHASES[index - 1]!.width,
  ],
  [],
);
const SETTLE_END =
  PHASE_START[PHASES.length - 1]! + PHASES[PHASES.length - 1]!.width;

/** Legend entries laid out left to right, sized by label length. */
const LEGEND_X = PHASES.reduce<number[]>(
  (xs, phase, index) => [
    ...xs,
    index === 0
      ? CHART.x
      : xs[index - 1]! + 30 + PHASES[index - 1]!.label.length * 6.4,
  ],
  [],
);

/** The phase each storyboard frame points at. */
const FRAME_PHASE: readonly PhaseKey[] = [
  "collect",
  "aggregate",
  "clear",
  "settle",
];

function BatchRow({
  batch,
  highlight,
}: {
  readonly batch: number;
  readonly highlight: PhaseKey;
}) {
  const x = batch * WINDOW;
  const y = batch * ROW_H;
  return (
    <g>
      {PHASES.map((phase, index) => {
        const start = x + PHASE_START[index]!;
        const active = phase.key === highlight;
        return (
          <g key={phase.key}>
            <rect
              x={start + 1}
              y={y}
              width={phase.width - 2}
              height={BAR_H}
              rx={5}
              className={`${phase.className} story-move`}
              style={{ opacity: active ? 1 : 0.35 }}
            />
            {phase.key === "settle" && (
              <rect
                x={x + SETTLE_END - 1}
                y={y}
                width={26}
                height={BAR_H}
                rx={5}
                className={`${phase.className} story-move`}
                style={{ opacity: active ? 0.35 : 0.12 }}
              />
            )}
          </g>
        );
      })}
      <text
        x={batch * WINDOW + 10}
        y={y + 15.5}
        className="fill-accent-ink font-mono text-[12px] font-medium"
      >
        #{FIRST_BATCH + batch}
      </text>
    </g>
  );
}

/**
 * A Gantt chart that scrolls one collection window per step: each batch
 * starts collecting the instant the previous one closes, while the previous
 * one aggregates, clears and settles in parallel.
 */
export function PipelineGanttDiagram({ frame, tick }: DiagramProps) {
  // Animate toward the end of the current window, so motion never pauses.
  const time = tick + 1;
  const highlight = FRAME_PHASE[frame] ?? "collect";
  const rows = Array.from({ length: 8 }, (_, offset) => tick - 4 + offset);

  return (
    <Stage>
      <defs>
        <clipPath id="gantt-clip">
          <rect
            x={CHART.x}
            y={CHART.y}
            width={CHART.w}
            height={CHART.h}
            rx={10}
          />
        </clipPath>
      </defs>

      <g className="text-[12px]">
        {PHASES.map((phase, index) => (
          <g key={phase.key} transform={`translate(${LEGEND_X[index]} 22)`}>
            <rect width={10} height={10} rx={2} className={phase.className} />
            <text
              x={15}
              y={9}
              className={phase.key === highlight ? "fill-text" : "fill-faint"}
            >
              {phase.label}
            </text>
          </g>
        ))}
      </g>

      <rect
        x={CHART.x}
        y={CHART.y}
        width={CHART.w}
        height={CHART.h}
        rx={10}
        className="fill-raised/30 stroke-line"
      />
      <g clipPath="url(#gantt-clip)">
        <g
          className="story-scroll"
          style={{
            transform: `translate(${NOW_X - time * WINDOW}px, ${NOW_ROW_Y - time * ROW_H}px)`,
          }}
        >
          {rows.map((batch) => (
            <BatchRow key={batch} batch={batch} highlight={highlight} />
          ))}
        </g>
      </g>

      <path
        d={`M${NOW_X} ${CHART.y - 4}V${CHART.y + CHART.h + 4}`}
        className="stroke-text"
        strokeWidth={1.5}
        strokeDasharray="4 3"
      />
      <text
        x={NOW_X}
        y={CHART.y - 8}
        textAnchor="middle"
        className="fill-text text-[12px] font-medium"
      >
        now
      </text>
      <text
        x={CHART.x + CHART.w}
        y={CHART.y + CHART.h + 22}
        textAnchor="end"
        className="fill-faint text-[12px]"
      >
        time →
      </text>
      <text
        x={CHART.x}
        y={CHART.y + CHART.h + 22}
        className="fill-muted text-[12px]"
      >
        one batch collecting, earlier ones finishing in parallel
      </text>
    </Stage>
  );
}
