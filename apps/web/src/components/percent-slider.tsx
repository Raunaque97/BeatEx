"use client";

const PRESETS = [25, 50, 75, 100] as const;

/** `percent` of `atoms`, rounded down. */
export function shareOf(atoms: bigint, percent: number): bigint {
  return (atoms * BigInt(Math.round(percent))) / 100n;
}

/** How much of `total` `part` is, as a whole percent from 0 to 100. */
export function percentOf(part: bigint, total: bigint): number {
  if (total <= 0n || part <= 0n) return 0;
  return part >= total ? 100 : Number((part * 100n) / total);
}

/** A 0–100% slider with 25 / 50 / 75 / Max stops, for sizing against a balance. */
export function PercentSlider({
  label,
  value,
  disabled = false,
  onChange,
}: {
  readonly label: string;
  readonly value: number;
  readonly disabled?: boolean;
  readonly onChange: (percent: number) => void;
}) {
  return (
    <div className="flex flex-col gap-2">
      <input
        aria-label={label}
        type="range"
        min={0}
        max={100}
        step={1}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(Number(event.target.value))}
        className="accent-accent w-full disabled:opacity-40"
      />
      <div className="grid grid-cols-4 gap-1.5">
        {PRESETS.map((preset) => (
          <button
            key={preset}
            type="button"
            disabled={disabled}
            onClick={() => onChange(preset)}
            aria-pressed={value === preset}
            className={`min-h-7 rounded-md border text-xs disabled:opacity-40 ${
              value === preset
                ? "border-accent/60 text-accent"
                : "border-line text-muted hover:text-text"
            }`}
          >
            {preset === 100 ? "Max" : `${preset}%`}
          </button>
        ))}
      </div>
    </div>
  );
}
