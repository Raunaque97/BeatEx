"use client";

import { CURVE_PRESETS, matchingPreset } from "../model/curve-presets";

/** A 17-bar thumbnail of a curve, drawn like the ladder: high prices on top. */
function ShapePreview({ levels }: { readonly levels: readonly number[] }) {
  const rows = [...levels].reverse();
  const row = 22 / rows.length;
  return (
    <svg aria-hidden="true" width={26} height={22} viewBox="0 0 26 22">
      <line
        x1={13}
        x2={13}
        y1={0}
        y2={22}
        stroke="currentColor"
        strokeOpacity={0.25}
      />
      {rows.map((percent, index) =>
        percent === 0 ? null : (
          <rect
            key={index}
            x={percent > 0 ? 13 : 13 - (Math.abs(percent) / 100) * 12}
            width={(Math.abs(percent) / 100) * 12}
            y={index * row + 0.15}
            height={Math.max(0.6, row - 0.3)}
            fill={percent > 0 ? "var(--color-buy)" : "var(--color-sell)"}
          />
        ),
      )}
    </svg>
  );
}

/** One-click starting shapes for a custom curve; any level can still be dragged after. */
export function CurvePresets({
  levels,
  onPick,
}: {
  readonly levels: readonly number[];
  readonly onPick: (levels: number[]) => void;
}) {
  const active = matchingPreset(levels)?.id;
  return (
    <div
      role="group"
      aria-label="Curve presets"
      className="grid grid-cols-6 gap-1.5"
    >
      {CURVE_PRESETS.map((preset) => (
        <button
          key={preset.id}
          type="button"
          title={`${preset.label}: ${preset.description}`}
          aria-label={`${preset.label}: ${preset.description}`}
          aria-pressed={active === preset.id}
          onClick={() => onPick(preset.levels())}
          className={`flex h-8 items-center justify-center rounded-md border ${
            active === preset.id
              ? "border-accent/60 text-accent bg-raised"
              : "border-line text-muted hover:text-text hover:border-line-strong"
          }`}
        >
          <ShapePreview levels={preset.levels()} />
        </button>
      ))}
    </div>
  );
}
