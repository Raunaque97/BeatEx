import type { ReactNode } from "react";

/** Every storyboard diagram draws into the same 480×300 canvas. */
export function Stage({ children }: { readonly children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 480 300"
      className="block h-auto w-full font-sans select-none"
      aria-hidden
    >
      {children}
    </svg>
  );
}

/** A group that glides to (`x`, `y`) and fades to `opacity` when a frame changes. */
export function Move({
  x,
  y,
  opacity = 1,
  scale = 1,
  delay = 0,
  children,
}: {
  readonly x: number;
  readonly y: number;
  readonly opacity?: number;
  readonly scale?: number;
  readonly delay?: number;
  readonly children: ReactNode;
}) {
  return (
    <g
      className="story-move"
      style={{
        transform: `translate(${x}px, ${y}px) scale(${scale})`,
        opacity,
        transitionDelay: `${delay}ms`,
      }}
    >
      {children}
    </g>
  );
}

/** Shows its children only while `when` holds, fading in and out. */
export function Fade({
  when,
  delay = 0,
  children,
}: {
  readonly when: boolean;
  readonly delay?: number;
  readonly children: ReactNode;
}) {
  return (
    <g
      className="story-move"
      style={{ opacity: when ? 1 : 0, transitionDelay: `${delay}ms` }}
    >
      {children}
    </g>
  );
}

/** A padlock centred on (`x`, `y`). */
export function Lock({
  x,
  y,
  size = 16,
  open = false,
  className = "stroke-accent",
}: {
  readonly x: number;
  readonly y: number;
  readonly size?: number;
  readonly open?: boolean;
  readonly className?: string;
}) {
  const s = size / 16;
  return (
    <g
      transform={`translate(${x - 8 * s} ${y - 8 * s}) scale(${s})`}
      className={className}
      fill="none"
      strokeWidth={1.8}
      strokeLinecap="round"
    >
      <rect x="2.5" y="7" width="11" height="8" rx="2" />
      <path
        d={open ? "M5 7V4.5a3 3 0 0 1 5.8-1" : "M5 7V4.5a3 3 0 0 1 6 0V7"}
      />
    </g>
  );
}

/** A pill with a check mark, for "proof verified" moments. */
export function CheckPill({
  x,
  y,
  width,
  label,
}: {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly label: string;
}) {
  return (
    <g transform={`translate(${x} ${y})`}>
      <rect
        width={width}
        height={26}
        rx={13}
        className="fill-accent/10 stroke-accent/60"
      />
      <path
        d="M12 13.5l3.5 3.5 7-8"
        fill="none"
        className="stroke-accent"
        strokeWidth={2}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <text x={30} y={17.5} className="fill-accent text-[12px] font-medium">
        {label}
      </text>
    </g>
  );
}

/** Horizontal centre of grid level `index` in a 17-level chart spanning 40–440. */
export function levelX(index: number, levels = 17): number {
  return 40 + (400 / levels) * (index + 0.5);
}
