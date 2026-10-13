"use client";

import Link from "next/link";

import { DEFAULT_MARKET_SLUG } from "@/lib/config/markets";

import { ClearingDiagram } from "./diagrams/clearing";
import { FrontRunDiagram } from "./diagrams/front-run";
import { PipelineGanttDiagram } from "./diagrams/pipeline-gantt";
import { SealBatchDiagram } from "./diagrams/seal-batch";
import { Heartbeat } from "./heartbeat";
import { StoryChapter } from "./story-chapter";

const TRADE_HREF = `/trade/${DEFAULT_MARKET_SLUG}` as const;

const CTA_CLASS =
  "bg-accent text-accent-ink inline-flex min-h-11 items-center gap-2 rounded-lg px-5 text-sm font-semibold transition hover:brightness-110";

function Arrow() {
  return (
    <svg viewBox="0 0 16 16" className="size-4" aria-hidden>
      <path
        d="M3 8h10M9 4l4 4-4 4"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.8}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function HomePage() {
  return (
    <div className="flex min-h-dvh flex-col">
      <header className="border-line bg-bg/85 sticky top-0 z-10 border-b backdrop-blur">
        <div className="mx-auto flex max-w-6xl items-center justify-between gap-4 px-4 py-3 sm:px-6">
          <Link href="/" className="text-lg font-bold tracking-tight">
            Beat<span className="text-accent">Ex</span>
          </Link>
          <nav className="flex items-center gap-2 sm:gap-5">
            <a
              href="#how"
              className="text-muted hover:text-text hidden text-sm sm:inline"
            >
              How it works
            </a>
            <Link
              href={TRADE_HREF}
              className="border-line-strong hover:border-accent inline-flex min-h-9 items-center rounded-lg border px-3.5 text-sm"
            >
              Launch app
            </Link>
          </nav>
        </div>
      </header>

      <main className="flex-1">
        <section className="mx-auto max-w-6xl px-4 pt-16 sm:px-6 sm:pt-24">
          <div className="text-accent font-mono text-xs tracking-[0.2em] uppercase">
            Live on Solana Devnet
          </div>
          <h1 className="mt-4 max-w-3xl text-4xl leading-[1.05] font-semibold tracking-tight text-balance sm:text-6xl">
            Trade without being front-run
          </h1>
          <p className="text-muted mt-5 max-w-2xl text-lg leading-relaxed">
            Sealed-bid batch auctions on Solana. Orders are encrypted, batched
            together, and cleared at one fair price.
          </p>
          <div className="mt-8 flex flex-wrap gap-3">
            <Link href={TRADE_HREF} className={CTA_CLASS}>
              Try it live on Solana Devnet
              <Arrow />
            </Link>
            <a
              href="#how"
              className="border-line-strong text-muted hover:text-text inline-flex min-h-11 items-center rounded-lg border px-5 text-sm"
            >
              How it works
            </a>
          </div>
        </section>
        <div className="mt-12 sm:mt-16">
          <Heartbeat />
        </div>

        <div id="how" className="border-line scroll-mt-14 border-t">
          <StoryChapter
            id="problem"
            number={1}
            kicker="The problem"
            title="On a public chain, everyone sees your order"
            diagramLabel="Your buy order is visible on its way to the block. A bot reads it, buys first to push the price up, and sells back to you at the higher price."
            Diagram={FrontRunDiagram}
            captions={[
              "You send a buy order. Anyone can read it while it waits.",
              "A bot spots it before it executes.",
              "The bot buys first and pushes the price up.",
              "You fill at the worse price; the bot sells back for a profit.",
            ]}
          >
            <p>
              On most on-chain exchanges a trade is visible before it executes.
              Whoever sees it first can trade ahead of it and sell back to you
              at a worse price.
            </p>
            <p>
              That&apos;s front-running. The fastest player wins, and you pay
              for it.
            </p>
          </StoryChapter>

          <StoryChapter
            id="seal"
            number={2}
            kicker="Seal and batch"
            title="Sealed orders, collected into one batch"
            diagramLabel="Your order is encrypted in the browser, a zero-knowledge proof shows it is valid and funded, and it joins other sealed orders in an open batch that then closes."
            Diagram={SealBatchDiagram}
            flip
            captions={[
              "Your order is written in your browser.",
              "It's encrypted, and a zero-knowledge proof shows it's valid and funded.",
              "It joins the open batch. Bots see only sealed orders.",
              "The batch closes. Arrival order doesn't matter.",
            ]}
          >
            <p>
              BeatEx encrypts your order before it leaves your browser. A
              zero-knowledge proof travels with it, so Solana can check the
              order is valid and fully funded without seeing a single number.
            </p>
            <p>
              Orders gather in a short batch. Nobody can read the batch while
              it&apos;s open, and arriving first earns nothing: everyone in it
              is treated the same.
            </p>
          </StoryChapter>

          <StoryChapter
            id="clear"
            number={3}
            kicker="One fair price"
            title="Add up sealed orders. Reveal only the totals."
            diagramLabel="Sealed orders are summed while encrypted. Three of seven key holders unlock only the totals, showing demand and supply at each price level. The price level with the most tradable volume becomes the clearing price."
            Diagram={ClearingDiagram}
            frameMs={2800}
            captions={[
              "Sealed orders are summed without being decrypted.",
              "3 of 7 key holders unlock only the totals.",
              "Total demand and supply at every price level.",
              "Tradable volume at each price is the smaller of the two.",
              "The price with the most volume becomes the clearing price.",
            ]}
          >
            <p>
              Each batch offers a ladder of prices around the last trade. Sealed
              orders are added together while still encrypted, so no single
              order is ever decrypted.
            </p>
            <p>
              When the batch closes, any 3 of 7 independent key holders jointly
              unlock just the totals. The contract then picks the price where
              the most volume trades, and anyone can verify it.
            </p>
          </StoryChapter>

          <StoryChapter
            id="settle"
            number={4}
            kicker="Settle and repeat"
            title="Settle privately. Then do it again."
            diagramLabel="A Gantt chart of consecutive batches. Each batch starts collecting the moment the previous one closes, while the previous one is aggregated, revealed and cleared, and settled in parallel."
            Diagram={PipelineGanttDiagram}
            flip
            continuous
            frameMs={2800}
            captions={[
              "Collect: sealed orders flow into the open batch.",
              "Aggregate: the closed batch is summed while the next one collects.",
              "Reveal and clear: the totals are unlocked and the price is set.",
              "Settle: traders privately claim their fills.",
            ]}
          >
            <p>
              Everyone in a batch trades at the same price. If one side wants
              more than the other offers, that side is filled pro-rata and the
              rest is returned.
            </p>
            <p>
              You prove your own fill and your encrypted balance updates; no one
              else learns what you traded. Withdraw to ordinary tokens any time.
            </p>
            <p>
              Batches run back to back. The next one opens the moment the last
              one closes, while the last one is cleared and settled alongside
              it.
            </p>
          </StoryChapter>
        </div>

        <section className="border-line border-t">
          <div className="mx-auto max-w-6xl px-4 py-20 text-center sm:px-6 sm:py-28">
            <h2 className="text-3xl font-semibold tracking-tight sm:text-5xl">
              Try it live on Solana Devnet
            </h2>
            <ol className="mx-auto mt-10 grid max-w-4xl gap-3 text-left sm:grid-cols-3">
              {[
                ["Open the app", "A demo wallet is created in your browser."],
                [
                  "Get test tokens",
                  "One click funds tBTC, tUSD, and SOL for fees.",
                ],
                ["Place an order", "Watch your batch seal, clear, and settle."],
              ].map(([title, body], index) => (
                <li
                  key={title}
                  className="border-line bg-panel rounded-xl border p-5"
                >
                  <div className="text-accent font-mono text-xs">
                    {index + 1}
                  </div>
                  <div className="mt-2 font-medium">{title}</div>
                  <div className="text-muted mt-1 text-sm">{body}</div>
                </li>
              ))}
            </ol>
            <Link href={TRADE_HREF} className={`${CTA_CLASS} mt-10`}>
              Launch app
              <Arrow />
            </Link>
          </div>
        </section>
      </main>

      <footer className="border-line text-faint border-t px-4 py-6 text-center text-xs sm:px-6">
        Prototype cryptography, not audited. Devnet test tokens only.
      </footer>
    </div>
  );
}
