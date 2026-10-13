import type { CSSProperties } from "react";

import {
  DEMO_CLEARING,
  DEMO_DEMAND,
  DEMO_GRID,
  DEMO_LEVELS,
  DEMO_SUPPLY,
} from "../../model/clearing-demo";
import type { DiagramProps } from "../story-chapter";
import { Fade, Lock, Stage, levelX } from "./stage";

const BASE_Y = 240;
const TOP_Y = 96;
const CHART_H = BASE_Y - TOP_Y;
const MAX_QTY = Math.max(...DEMO_DEMAND, ...DEMO_SUPPLY);
const SUM_BOX = { x: 170, y: 118, w: 140, h: 76 } as const;
const QUORUM = new Set([1, 3, 6]);
/** Where the sealed orders start before they fold into the sum. */
const SEALED_ORDERS = [60, 125, 190, 290, 355, 420] as const;
/** The scan line takes this long to find the winning level. */
const SCAN_MS = 1400;

const winnerX = levelX(DEMO_CLEARING.index);
const maxLineY = BASE_Y - (CHART_H * DEMO_CLEARING.volume) / MAX_QTY;

function scale(quantity: number): number {
  return quantity / MAX_QTY;
}

function formatPrice(price: number): string {
  return price.toLocaleString("en-US");
}

export function ClearingDiagram({ frame }: DiagramProps) {
  const unlocked = frame >= 1;
  const revealed = frame >= 2;
  const matched = frame >= 3;
  const cleared = frame >= 4;

  return (
    <Stage>
      {/* Legend grows with the story. */}
      <g className="text-[12px]">
        <Fade when={revealed}>
          <rect
            x={40}
            y={24}
            width={10}
            height={10}
            rx={2}
            className="fill-buy"
          />
          <text x={56} y={33} className="fill-muted">
            demand
          </text>
          <rect
            x={114}
            y={24}
            width={10}
            height={10}
            rx={2}
            className="fill-sell"
          />
          <text x={130} y={33} className="fill-muted">
            supply
          </text>
        </Fade>
        <Fade when={matched}>
          <rect
            x={186}
            y={24}
            width={10}
            height={10}
            rx={2}
            className="fill-accent"
          />
          <text x={202} y={33} className="fill-muted">
            tradable = smaller of the two
          </text>
        </Fade>
      </g>

      {/* The winning column and the max-volume line. */}
      <Fade when={cleared} delay={cleared ? SCAN_MS : 0}>
        <rect
          x={winnerX - 13}
          y={maxLineY - 8}
          width={26}
          height={BASE_Y - maxLineY + 8}
          rx={6}
          className="fill-accent/10 stroke-accent/60"
        />
        <path
          d={`M36 ${maxLineY}H444`}
          className="stroke-accent"
          strokeDasharray="6 5"
          strokeWidth={1.5}
        />
        <text
          x={444}
          y={maxLineY - 8}
          textAnchor="end"
          className="fill-accent text-[12px]"
        >
          max volume: {DEMO_CLEARING.volume}
        </text>
        <rect
          x={winnerX - 84}
          y={50}
          width={168}
          height={28}
          rx={14}
          className="fill-accent"
        />
        <text
          x={winnerX}
          y={69}
          textAnchor="middle"
          className="fill-accent-ink text-[13px] font-semibold"
        >
          Clearing price {formatPrice(DEMO_CLEARING.price)}
        </text>
        <path
          d={`M${winnerX} 78V${maxLineY - 8}`}
          className="stroke-accent"
          strokeWidth={1.5}
        />
      </Fade>

      {Array.from({ length: DEMO_LEVELS }, (_, index) => {
        const x = levelX(index);
        const demand = DEMO_DEMAND[index]!;
        const supply = DEMO_SUPPLY[index]!;
        const loser = cleared && index !== DEMO_CLEARING.index;
        const stagger = (active: boolean): CSSProperties => ({
          transitionDelay: active ? `${index * 30}ms` : "0ms",
        });
        return (
          <g key={index}>
            <rect
              x={x - 9}
              y={TOP_Y}
              width={8}
              height={CHART_H}
              rx={2}
              className="fill-buy story-bar"
              style={{
                transform: `scaleY(${revealed ? scale(demand) : 0})`,
                opacity: matched ? 0.3 : 1,
                ...stagger(revealed && !matched),
              }}
            />
            <rect
              x={x + 1}
              y={TOP_Y}
              width={8}
              height={CHART_H}
              rx={2}
              className="fill-sell story-bar"
              style={{
                transform: `scaleY(${revealed ? scale(supply) : 0})`,
                opacity: matched ? 0.3 : 1,
                ...stagger(revealed && !matched),
              }}
            />
            <rect
              x={x - 7}
              y={TOP_Y}
              width={14}
              height={CHART_H}
              rx={2}
              className="fill-accent story-bar"
              style={{
                transform: `scaleY(${matched ? scale(Math.min(demand, supply)) : 0})`,
                opacity: loser ? 0.45 : 1,
                transitionDelay:
                  matched && !cleared
                    ? `${index * 30}ms`
                    : cleared
                      ? `${SCAN_MS}ms`
                      : "0ms",
              }}
            />
          </g>
        );
      })}

      {/* The scan line sweeps across and stops on the tallest tradable bar. */}
      <g
        className="story-scan"
        style={{
          transform: `translateX(${cleared ? winnerX - 36 : 0}px)`,
          opacity: cleared ? 1 : 0,
        }}
      >
        <path
          d={`M36 ${TOP_Y - 6}V${BASE_Y}`}
          className="stroke-text/70"
          strokeWidth={1.5}
        />
      </g>

      <path d={`M36 ${BASE_Y}H444`} className="stroke-line-strong" />
      <g className="font-mono text-[12px]" textAnchor="middle">
        <text x={levelX(0)} y={260} className="fill-faint">
          {formatPrice(DEMO_GRID[0]!)}
        </text>
        <text x={levelX(DEMO_LEVELS - 1)} y={260} className="fill-faint">
          {formatPrice(DEMO_GRID[DEMO_LEVELS - 1]!)}
        </text>
        <Fade when={cleared} delay={cleared ? SCAN_MS : 0}>
          <text x={winnerX} y={260} className="fill-accent">
            {formatPrice(DEMO_CLEARING.price)}
          </text>
        </Fade>
      </g>
      <text
        x={240}
        y={286}
        textAnchor="middle"
        className="fill-faint text-[12px]"
      >
        price levels
      </text>

      {/* Before the reveal: sealed orders fold into one sealed sum. */}
      <Fade when={!revealed}>
        <rect
          x={36}
          y={TOP_Y - 50}
          width={408}
          height={BASE_Y - TOP_Y + 50}
          rx={14}
          className="fill-panel"
        />
        {frame === 0 &&
          SEALED_ORDERS.map((x, index) => (
            <g
              key={x}
              className="story-fold"
              style={
                {
                  "--fold-x": `${x - (SUM_BOX.x + SUM_BOX.w / 2)}px`,
                  "--fold-y": `${70 - (SUM_BOX.y + SUM_BOX.h / 2)}px`,
                  animationDelay: `${300 + index * 120}ms`,
                } as CSSProperties
              }
            >
              <g
                transform={`translate(${SUM_BOX.x + SUM_BOX.w / 2 - 22} ${SUM_BOX.y + SUM_BOX.h / 2 - 15})`}
              >
                <rect
                  width={44}
                  height={30}
                  rx={6}
                  className="fill-raised stroke-line-strong"
                />
                <Lock x={22} y={15} size={12} className="stroke-muted" />
              </g>
            </g>
          ))}
        <rect
          x={SUM_BOX.x}
          y={SUM_BOX.y}
          width={SUM_BOX.w}
          height={SUM_BOX.h}
          rx={12}
          className={`story-move ${unlocked ? "fill-raised stroke-accent" : "fill-raised stroke-line-strong"}`}
        />
        <text
          x={SUM_BOX.x + 26}
          y={SUM_BOX.y + 50}
          className="fill-text font-mono text-[30px]"
        >
          Σ
        </text>
        <text
          x={SUM_BOX.x + 56}
          y={SUM_BOX.y + 34}
          className="fill-text text-[12px] font-medium"
        >
          batch totals
        </text>
        <text
          x={SUM_BOX.x + 56}
          y={SUM_BOX.y + 52}
          className="fill-muted text-[12px]"
        >
          {unlocked ? "unlocked" : "still sealed"}
        </text>
        <circle
          cx={SUM_BOX.x + SUM_BOX.w}
          cy={SUM_BOX.y}
          r={15}
          className="fill-panel stroke-accent/60"
        />
        <Lock
          x={SUM_BOX.x + SUM_BOX.w}
          y={SUM_BOX.y}
          size={15}
          open={unlocked}
        />
        <Fade when={unlocked}>
          {Array.from({ length: 7 }, (_, index) => (
            <circle
              key={index}
              cx={186 + index * 18}
              cy={220}
              r={6}
              className={QUORUM.has(index) ? "fill-accent" : "fill-line-strong"}
            />
          ))}
          <text
            x={240}
            y={246}
            textAnchor="middle"
            className="fill-muted text-[12px]"
          >
            3 of 7 key holders unlock the totals
          </text>
        </Fade>
        <Fade when={!unlocked}>
          <text
            x={240}
            y={232}
            textAnchor="middle"
            className="fill-muted text-[12px]"
          >
            added together while encrypted
          </text>
        </Fade>
      </Fade>
    </Stage>
  );
}
