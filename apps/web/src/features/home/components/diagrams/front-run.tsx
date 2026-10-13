import type { DiagramProps } from "../story-chapter";
import { Fade, Move, Stage } from "./stage";

const SLOT_Y = [112, 172, 232] as const;
const BLOCK_X = 412;

function OrderCard({
  label,
  tone,
}: {
  readonly label: string;
  readonly tone: "buy" | "sell";
}) {
  return (
    <g>
      <rect
        x={-48}
        y={-20}
        width={96}
        height={40}
        rx={8}
        className={
          tone === "buy"
            ? "fill-buy-deep stroke-buy"
            : "fill-sell-deep stroke-sell"
        }
      />
      <text
        y={5}
        textAnchor="middle"
        className={`font-mono text-[13px] font-medium ${
          tone === "buy" ? "fill-buy-soft" : "fill-sell-soft"
        }`}
      >
        {label}
      </text>
    </g>
  );
}

const YOUR_ORDER: readonly (readonly [number, number])[] = [
  [140, 150],
  [228, 172],
  [252, 176],
  [BLOCK_X, SLOT_Y[1]],
];

const PRICE = [
  { value: "85,500", note: "what you expect", tone: "fill-text" },
  { value: "85,500", note: "what you expect", tone: "fill-text" },
  { value: "85,690 ▲", note: "bot pushed it up", tone: "fill-sell" },
  { value: "85,690", note: "what you paid", tone: "fill-sell" },
] as const;

export function FrontRunDiagram({ frame }: DiagramProps) {
  const [orderX, orderY] = YOUR_ORDER[frame] ?? YOUR_ORDER[0]!;
  const price = PRICE[frame] ?? PRICE[0];

  return (
    <Stage>
      {/* Price you expect versus price you get. */}
      <rect
        x={20}
        y={18}
        width={156}
        height={62}
        rx={10}
        className="fill-raised"
      />
      <text x={34} y={40} className="fill-muted text-[12px]">
        tBTC price · {price.note}
      </text>
      <text
        x={34}
        y={66}
        className={`font-mono text-[20px] font-medium ${price.tone}`}
      >
        {price.value}
      </text>

      {/* You. */}
      <circle cx={48} cy={150} r={22} className="fill-buy-deep stroke-buy" />
      <circle cx={48} cy={143} r={6} className="fill-buy-soft" />
      <path d="M36 162a12 10 0 0 1 24 0" className="fill-buy-soft" />
      <text
        x={48}
        y={194}
        textAnchor="middle"
        className="fill-muted text-[13px]"
      >
        You
      </text>

      {/* The next block, executed top to bottom. */}
      <text
        x={BLOCK_X}
        y={78}
        textAnchor="middle"
        className="fill-muted text-[13px]"
      >
        Next block
      </text>
      <rect
        x={356}
        y={86}
        width={112}
        height={172}
        rx={12}
        className="stroke-line-strong fill-none"
        strokeDasharray="4 4"
      />
      {SLOT_Y.map((y, index) => (
        <text
          key={y}
          x={348}
          y={y + 4}
          textAnchor="end"
          className="fill-faint font-mono text-[11px]"
        >
          {index + 1}
        </text>
      ))}

      {/* The bot watching pending transactions. */}
      <g transform="translate(232 56)">
        <rect
          x={-22}
          y={-20}
          width={44}
          height={36}
          rx={10}
          className="fill-sell-deep stroke-sell"
        />
        <circle cx={-8} cy={-3} r={4} className="fill-sell-soft" />
        <circle cx={8} cy={-3} r={4} className="fill-sell-soft" />
        <path d="M0 -20v-8" className="stroke-sell" strokeWidth={2} />
        <circle cy={-30} r={3} className="fill-sell" />
      </g>
      <text x={262} y={52} className="fill-sell-soft text-[13px]">
        Bot
      </text>
      <Fade when={frame === 3}>
        <text x={262} y={70} className="fill-accent font-mono text-[12px]">
          +profit
        </text>
      </Fade>
      <Fade when={frame === 1 || frame === 2}>
        <path
          d="M220 76L210 150M244 76L254 150"
          className="stroke-sell/70"
          strokeDasharray="3 4"
          strokeWidth={1.5}
        />
        <text
          x={300}
          y={118}
          textAnchor="middle"
          className="fill-sell-soft text-[12px]"
        >
          reads it
        </text>
      </Fade>

      {/* Bot jumps ahead, then sells back to you. */}
      <Move
        x={frame >= 2 ? BLOCK_X : 232}
        y={frame >= 2 ? SLOT_Y[0] : 60}
        opacity={frame >= 2 ? 1 : 0}
        scale={frame >= 2 ? 1 : 0.5}
      >
        <OrderCard label="BOT BUY" tone="sell" />
      </Move>
      <Move
        x={frame >= 3 ? BLOCK_X : 232}
        y={frame >= 3 ? SLOT_Y[2] : 60}
        opacity={frame >= 3 ? 1 : 0}
        scale={frame >= 3 ? 1 : 0.5}
        delay={frame >= 3 ? 400 : 0}
      >
        <OrderCard label="BOT SELL" tone="sell" />
      </Move>

      {/* Your order, readable by anyone on its way. */}
      <Move x={orderX} y={orderY}>
        <OrderCard label="BUY 2 tBTC" tone="buy" />
      </Move>
    </Stage>
  );
}
