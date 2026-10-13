import type { DiagramProps } from "../story-chapter";
import { CheckPill, Fade, Lock, Move, Stage } from "./stage";

const CARD = { x: 22, y: 74, w: 180, h: 120 } as const;
const BOX = { x: 250, y: 74, w: 210, h: 172 } as const;
const CHIP_W = 60;
const CHIP_H = 40;
/** The order card shrinks to exactly one batch slot. */
const CHIP_SCALE = CHIP_W / CARD.w;
/** How far the batch window has run in each frame. */
const PROGRESS = [0.25, 0.45, 0.7, 1] as const;
/** Other traders' sealed orders, in arrival order. */
const OTHERS: readonly (readonly [number, number])[] = [
  [0, 0],
  [2, 1],
  [1, 0],
  [0, 2],
  [2, 0],
  [0, 1],
  [2, 2],
  [1, 2],
];
const YOUR_SLOT = [1, 1] as const;
const STACK = { x: BOX.x + 75, y: BOX.y + 66 } as const;

const PLAIN = [
  { text: "Buy  0.5 tBTC", tone: "fill-buy-soft" },
  { text: "  at ≤ 85,415", tone: "fill-buy-soft" },
  { text: "Sell 0.3 tBTC", tone: "fill-sell-soft" },
  { text: "  at ≥ 85,757", tone: "fill-sell-soft" },
] as const;

const CIPHER = [
  "9f3a 71c2 e04b",
  "d81e 0a96 5c3f",
  "37bb e2d0 81af",
  "c4e9 1f57 0b2d",
] as const;

function slotPosition([column, row]: readonly [number, number]) {
  return { x: BOX.x + 12 + column * 66, y: BOX.y + 12 + row * 52 };
}

function SealedChip() {
  return (
    <g>
      <rect
        width={CHIP_W}
        height={CHIP_H}
        rx={7}
        className="fill-raised stroke-line-strong"
      />
      <Lock x={15} y={20} size={13} className="stroke-muted" />
      {[13, 19, 25].map((y, index) => (
        <rect
          key={y}
          x={28}
          y={y}
          width={22 - index * 5}
          height={3}
          rx={1.5}
          className="fill-faint"
        />
      ))}
    </g>
  );
}

export function SealBatchDiagram({ frame }: DiagramProps) {
  const sealed = frame >= 1;
  const submitted = frame >= 2;
  const closed = frame >= 3;
  const arrived = [2, 4, 6, 8][frame] ?? 0;

  const yourSlot = slotPosition(YOUR_SLOT);
  const card = closed
    ? { x: STACK.x - 6, y: STACK.y - 6, scale: CHIP_SCALE }
    : submitted
      ? { ...yourSlot, scale: CHIP_SCALE }
      : { x: CARD.x, y: CARD.y, scale: 1 };

  return (
    <Stage>
      {/* The batch: a window that fills, then closes. */}
      <text x={BOX.x} y={40} className="fill-text text-[13px] font-medium">
        {closed ? "Batch closed" : "Batch open"}
      </text>
      <rect
        x={BOX.x}
        y={50}
        width={BOX.w}
        height={8}
        rx={4}
        className="fill-line"
      />
      <rect
        x={BOX.x}
        y={50}
        width={BOX.w}
        height={8}
        rx={4}
        className="fill-accent story-bar-x"
        style={{ transform: `scaleX(${PROGRESS[frame] ?? 1})` }}
      />
      <rect
        x={BOX.x}
        y={BOX.y}
        width={BOX.w}
        height={BOX.h}
        rx={14}
        className={`story-move ${
          closed
            ? "fill-raised/50 stroke-accent/50"
            : "fill-raised/30 stroke-line-strong"
        }`}
        strokeDasharray={closed ? undefined : "5 5"}
      />
      <Fade when={closed}>
        <circle
          cx={BOX.x + BOX.w}
          cy={BOX.y}
          r={14}
          className="fill-panel stroke-accent/50"
        />
        <Lock x={BOX.x + BOX.w} y={BOX.y} size={14} />
        <text
          x={BOX.x + BOX.w / 2}
          y={BOX.y + 150}
          textAnchor="middle"
          className="fill-muted text-[12px]"
        >
          one batch, no queue
        </text>
      </Fade>

      {OTHERS.map((slot, index) => {
        const position = closed
          ? { x: STACK.x + (index - 4) * 3, y: STACK.y + (index - 4) * 3 }
          : slotPosition(slot);
        return (
          <Move
            key={index}
            x={position.x}
            y={position.y}
            opacity={index < arrived ? 1 : 0}
            delay={closed ? index * 50 : (index % 2) * 500}
          >
            <SealedChip />
          </Move>
        );
      })}

      {/* Your order: readable in the browser, sealed before it leaves. */}
      <Fade when={!submitted}>
        <text x={CARD.x} y={62} className="fill-muted text-[12px]">
          {sealed ? "Encrypted before it leaves" : "In your browser"}
        </text>
      </Fade>
      <Fade when={frame === 1}>
        <path
          d={`M${CARD.x + CARD.w + 10} ${CARD.y + CARD.h / 2}H${BOX.x - 10}`}
          className="stroke-accent/60"
          strokeDasharray="3 4"
          strokeWidth={1.5}
        />
      </Fade>
      <Move x={card.x} y={card.y} scale={card.scale}>
        <rect
          width={CARD.w}
          height={CARD.h}
          rx={12}
          className={`story-move ${
            sealed
              ? "fill-raised stroke-accent"
              : "fill-raised stroke-line-strong"
          }`}
          strokeWidth={submitted ? 3 : 1}
        />
        {PLAIN.map((line, index) => (
          <g key={line.text}>
            <Fade when={!sealed} delay={sealed ? index * 60 : 300}>
              <text
                x={16}
                y={30 + index * 24}
                className={`font-mono text-[14px] ${line.tone}`}
                style={{ whiteSpace: "pre" }}
              >
                {line.text}
              </text>
            </Fade>
            <Fade when={sealed} delay={sealed ? 200 + index * 90 : 0}>
              <text
                x={16}
                y={30 + index * 24}
                className="fill-faint font-mono text-[14px]"
              >
                {CIPHER[index]}
              </text>
            </Fade>
          </g>
        ))}
        <Fade when={sealed}>
          <circle
            cx={CARD.w}
            cy={0}
            r={15}
            className="fill-panel stroke-accent"
          />
          <Lock x={CARD.w} y={0} size={15} />
        </Fade>
      </Move>
      <Fade when={submitted && !closed} delay={600}>
        <text
          x={yourSlot.x + CHIP_W / 2}
          y={yourSlot.y + CHIP_H + 14}
          textAnchor="middle"
          className="fill-accent text-[11px]"
        >
          yours
        </text>
      </Fade>

      {/* The proof that travels with it. */}
      <Fade when={frame === 1}>
        <CheckPill
          x={CARD.x}
          y={208}
          width={200}
          label="ZK proof: valid and funded"
        />
        <text x={CARD.x} y={256} className="fill-muted text-[12px]">
          proves the rules hold,
        </text>
        <text x={CARD.x} y={272} className="fill-muted text-[12px]">
          reveals none of the numbers
        </text>
      </Fade>
      <Fade when={submitted} delay={400}>
        <CheckPill x={CARD.x} y={110} width={200} label="checked by Solana" />
        <text x={CARD.x} y={162} className="fill-muted text-[12px]">
          accepted into the batch,
        </text>
        <text x={CARD.x} y={178} className="fill-muted text-[12px]">
          contents still sealed
        </text>
      </Fade>

      {/* What an observer gets. */}
      <Fade when={submitted} delay={800}>
        <g transform={`translate(${BOX.x + 18} 270)`}>
          <rect
            x={-16}
            y={-13}
            width={32}
            height={26}
            rx={8}
            className="fill-sell-deep stroke-sell"
          />
          <circle cx={-6} cy={0} r={3} className="fill-sell-soft" />
          <circle cx={6} cy={0} r={3} className="fill-sell-soft" />
        </g>
        <text x={BOX.x + 44} y={275} className="fill-sell-soft text-[12px]">
          bots see only sealed orders
        </text>
      </Fade>
    </Stage>
  );
}
