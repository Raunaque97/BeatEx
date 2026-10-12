"use client";

import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type PointerEvent,
} from "react";

import { formatBase, formatPrice } from "@/lib/format";

import type { BatchSummary } from "../model/batch-summary";
import { priceDomain, toChartPoints } from "../model/chart-series";
import type { CurrentBatch } from "../model/current-batch";
import {
  formatTick,
  placeInTime,
  secondsPerAuction,
  timeTicks,
  toCandles,
  type Candle,
  type TimedPoint,
} from "../model/price-timeline";
import { BatchReveal } from "./batch-reveal";

const AXIS_WIDTH = 64;
const TOP = 10;
const TIME_AXIS = 20;
const VOLUME_HEIGHT = 40;
const GAP = 10;
/** Room right of "now" so the live auction isn't pinned to the edge. */
const LIVE_MARGIN = 0.1;
const DEFAULT_SPAN = 10 * 60;
const MIN_SPAN = 2 * 60;
const MAX_SPAN = 24 * 60 * 60;
/** Below this many pixels per auction, points give way to candles. */
const MIN_POINT_PX = 6;
/** Candle sizes to step through as the view widens; 1 minute first. */
const CANDLE_SECONDS = [60, 300, 900, 3_600];
const MIN_CANDLE_PX = 5;

const mono = { fontFamily: "var(--font-mono)" };

/** A manual price scale: the axis centre and half its range, in price units. */
interface PriceView {
  readonly center: number;
  readonly half: number;
}

interface Drag {
  /** Dragging the chart pans; dragging the price axis rescales it. */
  readonly mode: "pan" | "scale";
  readonly x: number;
  readonly y: number;
  readonly end: number;
  readonly center: number;
  readonly half: number;
  /** Whether a pan has moved far enough vertically to move prices too. */
  vertical: boolean;
}

/** Vertical travel before a pan also moves prices, so sideways drags keep auto-scale. */
const VERTICAL_PAN_THRESHOLD = 6;

interface View {
  readonly span: number;
  /** Right edge in unix seconds, or null to follow the live auction. */
  readonly end: number | null;
}

type Hover =
  | { readonly kind: "point"; readonly point: TimedPoint; readonly x: number }
  | { readonly kind: "candle"; readonly candle: Candle; readonly x: number };

/**
 * Price per auction over time, from the indexer and pending auction accounts,
 * with the live auction's price band. Scroll or pinch to zoom, drag to pan;
 * zoomed out, auctions merge into 1-minute candles. Hovering a column shows
 * what that auction (or minute) revealed.
 */
export function PriceChart({
  history,
  older,
  onNeedOlder,
  batch,
  lastClearingPrice,
  limitPrice,
}: {
  readonly history: readonly BatchSummary[];
  /** Older auctions loaded on demand; merged with `history` by id. */
  readonly older: readonly BatchSummary[];
  /** Ask for an older page once the view nears the oldest loaded auction. */
  readonly onNeedOlder: () => void;
  readonly batch: CurrentBatch | null;
  readonly lastClearingPrice: bigint | null;
  readonly limitPrice: bigint | null;
}) {
  const box = useRef<HTMLDivElement>(null);
  const svg = useRef<SVGSVGElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  const [view, setView] = useState<View>({ span: DEFAULT_SPAN, end: null });
  const [hover, setHover] = useState<Hover | null>(null);
  const [now, setNow] = useState(() => Date.now() / 1_000);
  const [priceView, setPriceView] = useState<PriceView | null>(null);
  const drag = useRef<Drag | null>(null);

  useLayoutEffect(() => {
    const element = box.current;
    if (element === null) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry === undefined) return;
      setSize({
        width: Math.round(entry.contentRect.width),
        height: Math.round(entry.contentRect.height),
      });
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // Keep "now" moving with the live auction.
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now() / 1_000), 1_000);
    return () => clearInterval(timer);
  }, []);

  const byId = new Map<bigint, BatchSummary>();
  for (const auction of [...older, ...history])
    byId.set(auction.auctionId, auction);
  const raw = toChartPoints(byId.values(), Number.MAX_SAFE_INTEGER);
  const rate = secondsPerAuction(raw);
  const points = placeInTime(
    raw,
    batch === null ? null : { auctionId: batch.auctionId, now },
  );
  const oldest = points[0]?.time ?? now;

  const liveEnd = now + view.span * LIVE_MARGIN;
  const end = view.end ?? liveEnd;
  const start = end - view.span;
  const plotLeft = 0;
  const plotRight = Math.max(1, size.width - AXIS_WIDTH);
  const plotWidth = plotRight - plotLeft;
  const volumeBottom = size.height - TIME_AXIS;
  const priceBottom = volumeBottom - VOLUME_HEIGHT - GAP;

  // Fetch older history before the view reaches the oldest loaded auction.
  useEffect(() => {
    if (start < oldest + view.span * 0.25) onNeedOlder();
  }, [start, oldest, view.span, onNeedOlder]);

  // Wheel zooms around the cursor (pinch arrives as ctrl+wheel); sideways
  // scroll pans. Registered natively: React's wheel listener is passive.
  useEffect(() => {
    const element = svg.current;
    if (element === null) return;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const rect = element.getBoundingClientRect();
      const fraction = Math.min(
        1,
        Math.max(0, (event.clientX - rect.left) / Math.max(1, plotWidth)),
      );
      setView((current) => {
        const currentEnd =
          current.end ?? Date.now() / 1_000 + current.span * LIVE_MARGIN;
        if (Math.abs(event.deltaX) > Math.abs(event.deltaY)) {
          return clampView(
            current.span,
            currentEnd + (event.deltaX / Math.max(1, plotWidth)) * current.span,
          );
        }
        const span = Math.min(
          MAX_SPAN,
          Math.max(MIN_SPAN, current.span * Math.exp(event.deltaY * 0.002)),
        );
        // Keep the time under the cursor fixed while zooming.
        const anchor = currentEnd - current.span * (1 - fraction);
        return clampView(span, anchor + span * (1 - fraction));
      });
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  }, [plotWidth]);

  if (size.width === 0) {
    return <div ref={box} className="h-full min-h-[260px] w-full" />;
  }

  const x = (time: number) =>
    plotLeft + ((time - start) / view.span) * plotWidth;
  const timeAt = (px: number) =>
    start + ((px - plotLeft) / plotWidth) * view.span;

  const visible = points.filter(
    (point) => point.time >= start - rate && point.time <= end + rate,
  );
  const pxPerAuction = (plotWidth * rate) / view.span;
  const candleSeconds =
    pxPerAuction >= MIN_POINT_PX
      ? null
      : (CANDLE_SECONDS.find(
          (seconds) => (plotWidth * seconds) / view.span >= MIN_CANDLE_PX,
        ) ?? CANDLE_SECONDS.at(-1)!);
  const candles =
    candleSeconds === null ? [] : toCandles(visible, candleSeconds);

  const grid = batch?.grid ?? null;
  const live = batch?.referencePrice ?? null;
  const liveVisible = now >= start && now <= end;
  const autoDomain =
    priceDomain([
      ...visible.map((point) => point.price),
      ...(liveVisible && grid !== null
        ? [grid[0]!.price, grid[grid.length - 1]!.price]
        : []),
      ...(liveVisible && live !== null ? [live] : []),
    ]) ??
    priceDomain(points.map((point) => point.price)) ??
    (live === null ? null : priceDomain([live]));
  const domain =
    priceView === null
      ? autoDomain
      : {
          min: priceView.center - priceView.half,
          max: priceView.center + priceView.half,
        };

  const y = (price: bigint) =>
    domain === null
      ? priceBottom
      : TOP +
        ((domain.max - Number(price)) / (domain.max - domain.min)) *
          (priceBottom - TOP);
  const priceTicks =
    domain === null
      ? []
      : Array.from({ length: 5 }, (_, index) =>
          BigInt(
            Math.round(
              domain.min + ((index + 0.5) * (domain.max - domain.min)) / 5,
            ),
          ),
        );
  const ticks = timeTicks(start, end, Math.max(2, Math.floor(plotWidth / 90)));
  const tickStep = ticks.length > 1 ? ticks[1]! - ticks[0]! : 60;

  const volumes =
    candleSeconds === null
      ? visible.map((point) => point.volume)
      : candles.map((candle) => candle.volume);
  const maxVolume = volumes.reduce((max, v) => (v > max ? v : max), 0n);
  const barHeight = (volume: bigint) =>
    maxVolume === 0n || volume === 0n
      ? 2
      : Math.max(3, (Number(volume) / Number(maxVolume)) * VOLUME_HEIGHT);
  const last = visible.at(-1);

  const findHover = (px: number, py: number): Hover | null => {
    // Only the volume strip reveals an auction; the price area stays clear.
    if (
      px > plotRight ||
      py < volumeBottom - VOLUME_HEIGHT ||
      py > volumeBottom
    )
      return null;
    const time = timeAt(px);
    if (candleSeconds !== null) {
      const candle = candles.find(
        (item) => time >= item.time && time < item.time + candleSeconds,
      );
      return candle === undefined
        ? null
        : { kind: "candle", candle, x: x(candle.time + candleSeconds / 2) };
    }
    let best: TimedPoint | null = null;
    for (const point of visible) {
      if (
        best === null ||
        Math.abs(point.time - time) < Math.abs(best.time - time)
      ) {
        best = point;
      }
    }
    return best === null || Math.abs(best.time - time) > rate
      ? null
      : { kind: "point", point: best, x: x(best.time) };
  };

  const onPointerDown = (event: PointerEvent<SVGSVGElement>) => {
    event.currentTarget.setPointerCapture(event.pointerId);
    const rect = event.currentTarget.getBoundingClientRect();
    const onAxis = event.clientX - rect.left > plotRight;
    drag.current = {
      mode: onAxis ? "scale" : "pan",
      x: event.clientX,
      y: event.clientY,
      end,
      center: domain === null ? 0 : (domain.min + domain.max) / 2,
      half: domain === null ? 0 : (domain.max - domain.min) / 2,
      vertical: priceView !== null,
    };
    setHover(null);
  };
  const onPointerMove = (event: PointerEvent<SVGSVGElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const current = drag.current;
    if (current !== null && event.buttons === 1) {
      const dy = event.clientY - current.y;
      if (current.mode === "scale") {
        // Drag down to compress prices, up to stretch them.
        if (current.half > 0) {
          setPriceView({
            center: current.center,
            half: clampHalf(current.half * Math.exp(dy / 200), current.center),
          });
        }
        return;
      }
      const shift = ((event.clientX - current.x) / plotWidth) * view.span;
      setView(clampView(view.span, current.end - shift));
      if (!current.vertical && Math.abs(dy) > VERTICAL_PAN_THRESHOLD) {
        current.vertical = true;
      }
      if (current.vertical && current.half > 0) {
        const pricePerPixel = (2 * current.half) / (priceBottom - TOP);
        setPriceView({
          center: current.center + dy * pricePerPixel,
          half: current.half,
        });
      }
      return;
    }
    setHover(findHover(event.clientX - rect.left, event.clientY - rect.top));
  };
  const onPointerUp = () => {
    drag.current = null;
  };

  const zoomBy = (factor: number) =>
    setView((current) => {
      const span = Math.min(
        MAX_SPAN,
        Math.max(MIN_SPAN, current.span * factor),
      );
      return current.end === null
        ? { span, end: null }
        : clampView(span, current.end);
    });

  return (
    <div ref={box} className="relative h-full min-h-[260px] w-full">
      <svg
        ref={svg}
        width={size.width}
        height={size.height}
        role="img"
        aria-label="Price per auction over time, with the live auction's price band"
        className="block cursor-grab touch-none select-none active:cursor-grabbing"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerLeave={() => setHover(null)}
        onDoubleClick={(event) => {
          const rect = event.currentTarget.getBoundingClientRect();
          // On the price axis: back to auto-scale. On the chart: reset both.
          setPriceView(null);
          if (event.clientX - rect.left <= plotRight) {
            setView({ span: DEFAULT_SPAN, end: null });
          }
        }}
      >
        {liveVisible && grid !== null ? (
          <>
            <rect
              x={plotLeft}
              width={plotWidth}
              y={y(grid[grid.length - 1]!.price)}
              height={y(grid[0]!.price) - y(grid[grid.length - 1]!.price)}
              fill="var(--color-accent)"
              fillOpacity={0.04}
            />
            <rect
              x={x(now) - Math.max(4, ((rate / view.span) * plotWidth) / 2)}
              width={Math.max(8, (rate / view.span) * plotWidth)}
              y={y(grid[grid.length - 1]!.price)}
              height={y(grid[0]!.price) - y(grid[grid.length - 1]!.price)}
              fill="var(--color-accent)"
              fillOpacity={0.1}
              stroke="var(--color-accent)"
              strokeOpacity={0.45}
              strokeDasharray="4 4"
            />
          </>
        ) : null}

        {priceTicks.map((tick) => (
          <g key={tick.toString()}>
            <line
              x1={plotLeft}
              x2={plotRight}
              y1={y(tick)}
              y2={y(tick)}
              stroke="var(--color-line)"
            />
            <text
              x={size.width - 6}
              y={y(tick) + 4}
              fill="var(--color-faint)"
              fontSize={11}
              textAnchor="end"
              style={mono}
            >
              {formatPrice(tick)}
            </text>
          </g>
        ))}

        {ticks.map((tick) => (
          <g key={tick}>
            <line
              x1={x(tick)}
              x2={x(tick)}
              y1={TOP}
              y2={volumeBottom}
              stroke="var(--color-line)"
              strokeOpacity={0.5}
            />
            <text
              x={x(tick)}
              y={size.height - 5}
              fill="var(--color-faint)"
              fontSize={11}
              textAnchor="middle"
              style={mono}
            >
              {formatTick(tick, tickStep)}
            </text>
          </g>
        ))}

        {lastClearingPrice !== null && lastClearingPrice > 0n ? (
          <line
            x1={plotLeft}
            x2={plotRight}
            y1={y(lastClearingPrice)}
            y2={y(lastClearingPrice)}
            stroke="var(--color-buy)"
            strokeDasharray="5 5"
            strokeOpacity={0.8}
          />
        ) : null}

        {candleSeconds === null ? (
          <>
            {visible.length > 1 ? (
              <polyline
                points={visible
                  .map((point) => `${x(point.time)},${y(point.price)}`)
                  .join(" ")}
                fill="none"
                stroke="#9fb4c2"
                strokeWidth={1.6}
              />
            ) : null}
            {visible.map((point) => (
              <circle
                key={point.auctionId.toString()}
                cx={x(point.time)}
                cy={y(point.price)}
                r={
                  hover?.kind === "point" &&
                  hover.point.auctionId === point.auctionId
                    ? 4
                    : 2.2
                }
                fill={
                  point.source === "clearing" ? "var(--color-text)" : "#9fb4c2"
                }
              />
            ))}
          </>
        ) : (
          candles.map((candle) => {
            const width = Math.max(
              2,
              ((candleSeconds * 0.7) / view.span) * plotWidth,
            );
            const center = x(candle.time + candleSeconds / 2);
            const up = candle.close >= candle.open;
            const color = up ? "var(--color-buy)" : "var(--color-sell)";
            const top = y(up ? candle.close : candle.open);
            const bottom = y(up ? candle.open : candle.close);
            return (
              <g key={candle.time}>
                <line
                  x1={center}
                  x2={center}
                  y1={y(candle.high)}
                  y2={y(candle.low)}
                  stroke={color}
                />
                <rect
                  x={center - width / 2}
                  width={width}
                  y={top}
                  height={Math.max(1, bottom - top)}
                  fill={color}
                />
              </g>
            );
          })
        )}

        {liveVisible && last !== undefined && live !== null ? (
          <line
            x1={x(last.time)}
            y1={y(last.price)}
            x2={x(now)}
            y2={y(live)}
            stroke="#9fb4c2"
            strokeDasharray="3 4"
          />
        ) : null}

        {candleSeconds === null
          ? visible.map((point) => {
              const height = barHeight(point.volume);
              const hovered =
                hover?.kind === "point" &&
                hover.point.auctionId === point.auctionId;
              return (
                <rect
                  key={point.auctionId.toString()}
                  x={x(point.time) - Math.min(3, pxPerAuction * 0.35)}
                  width={Math.max(2, Math.min(6, pxPerAuction * 0.7))}
                  y={volumeBottom - height}
                  height={height}
                  fill={
                    hovered
                      ? "var(--color-accent)"
                      : point.volume > 0n
                        ? "#3e5566"
                        : "var(--color-line-strong)"
                  }
                />
              );
            })
          : candles.map((candle) => {
              const height = barHeight(candle.volume);
              const width = Math.max(
                2,
                ((candleSeconds * 0.7) / view.span) * plotWidth,
              );
              const hovered =
                hover?.kind === "candle" && hover.candle.time === candle.time;
              return (
                <rect
                  key={candle.time}
                  x={x(candle.time + candleSeconds / 2) - width / 2}
                  width={width}
                  y={volumeBottom - height}
                  height={height}
                  fill={hovered ? "var(--color-accent)" : "#3e5566"}
                />
              );
            })}

        {liveVisible && live !== null ? (
          <circle cx={x(now)} cy={y(live)} r={4} fill="var(--color-accent)" />
        ) : null}
        {live !== null ? (
          <>
            <rect
              x={plotRight + 4}
              y={y(live) - 10}
              width={AXIS_WIDTH - 8}
              height={20}
              rx={3}
              fill="var(--color-buy-deep)"
            />
            <text
              x={plotRight + AXIS_WIDTH / 2}
              y={y(live) + 4}
              fill="#cfe6ff"
              fontSize={11}
              textAnchor="middle"
              style={mono}
            >
              {formatPrice(live)}
            </text>
          </>
        ) : null}

        {limitPrice !== null ? (
          <line
            x1={plotLeft}
            x2={plotRight}
            y1={y(limitPrice)}
            y2={y(limitPrice)}
            stroke="var(--color-accent)"
            strokeWidth={1.5}
            strokeDasharray="2 3"
          />
        ) : null}

        <line
          x1={plotLeft}
          x2={plotRight}
          y1={volumeBottom}
          y2={volumeBottom}
          stroke="var(--color-line)"
        />
        {hover !== null ? (
          <line
            x1={hover.x}
            x2={hover.x}
            y1={TOP}
            y2={volumeBottom}
            stroke="var(--color-muted)"
            strokeOpacity={0.5}
            pointerEvents="none"
          />
        ) : null}
        {points.length === 0 ? (
          <text
            x={plotWidth / 2}
            y={(TOP + priceBottom) / 2}
            fill="var(--color-muted)"
            fontSize={13}
            textAnchor="middle"
          >
            Loading price history…
          </text>
        ) : null}
        <rect
          x={plotRight}
          y={0}
          width={AXIS_WIDTH}
          height={volumeBottom}
          fill="transparent"
          style={{ cursor: "ns-resize" }}
        >
          <title>Drag to scale prices · double-click for auto</title>
        </rect>
      </svg>

      {priceView !== null ? (
        <button
          type="button"
          onClick={() => setPriceView(null)}
          title="Fit prices to the visible auctions"
          className="border-accent/50 bg-panel/80 text-accent absolute rounded-md border px-2 text-xs leading-6"
          style={{ right: 6, bottom: TIME_AXIS + 6 }}
        >
          Auto
        </button>
      ) : null}

      <div className="absolute top-2 left-2 flex gap-1">
        {(
          [
            ["−", "Zoom out", () => zoomBy(1.6)],
            ["+", "Zoom in", () => zoomBy(1 / 1.6)],
          ] as const
        ).map(([label, name, onClick]) => (
          <button
            key={name}
            type="button"
            aria-label={name}
            onClick={onClick}
            className="border-line bg-panel/80 text-muted hover:text-text h-7 w-7 rounded-md border text-sm"
          >
            {label}
          </button>
        ))}
        {view.end !== null ? (
          <button
            type="button"
            onClick={() => setView((current) => ({ ...current, end: null }))}
            className="border-accent/50 bg-panel/80 text-accent h-7 rounded-md border px-2 text-xs"
          >
            Live →
          </button>
        ) : null}
      </div>

      {hover !== null ? (
        <div
          role="tooltip"
          className="border-line-strong bg-panel pointer-events-none absolute z-10 rounded-lg border p-2.5 shadow-xl shadow-black/50"
          style={{
            top: TOP + 4,
            ...(hover.x > plotWidth / 2
              ? { right: size.width - hover.x + 12 }
              : { left: hover.x + 12 }),
          }}
        >
          {hover.kind === "point" ? (
            <>
              <p className="text-faint mb-1 font-mono text-[10px]">
                {formatTick(hover.point.time, 1)}
              </p>
              <BatchReveal auction={hover.point.auction} />
            </>
          ) : (
            <CandleCard candle={hover.candle} seconds={candleSeconds ?? 60} />
          )}
        </div>
      ) : null}
    </div>
  );
}

/** Keep a manual price range between 0.02% and 100% of the price. */
function clampHalf(half: number, center: number): number {
  const reference = Math.max(1, Math.abs(center));
  return Math.min(reference * 0.5, Math.max(reference * 0.0001, half));
}

/** Keep a panned view from running past the live edge. */
function clampView(span: number, end: number): View {
  const liveEnd = Date.now() / 1_000 + span * LIVE_MARGIN;
  return end >= liveEnd ? { span, end: null } : { span, end };
}

function CandleCard({
  candle,
  seconds,
}: {
  readonly candle: Candle;
  readonly seconds: number;
}) {
  const rows: readonly (readonly [string, string])[] = [
    ["O", formatPrice(candle.open)],
    ["H", formatPrice(candle.high)],
    ["L", formatPrice(candle.low)],
    ["C", formatPrice(candle.close)],
    ["Vol", `${formatBase(candle.volume)} tBTC`],
  ];
  return (
    <div className="w-[150px] font-mono text-xs">
      <p className="text-faint mb-1 text-[10px]">
        {formatTick(candle.time, 60)}–{formatTick(candle.time + seconds, 60)}
      </p>
      {rows.map(([label, value]) => (
        <p key={label} className="flex justify-between gap-3">
          <span className="text-faint">{label}</span>
          <span>{value}</span>
        </p>
      ))}
    </div>
  );
}
