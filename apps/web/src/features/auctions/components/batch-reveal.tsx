"use client";

import { AuctionState, buildAuctionGrid } from "@beatex/sdk";

import { formatBase, formatPrice } from "@/lib/format";

import type { BatchSummary } from "../model/batch-summary";

/**
 * A compact hover card: what one auction cleared, and the only data it ever
 * decrypted — the demand and supply totals at each price.
 */
export function BatchReveal({ auction }: { readonly auction: BatchSummary }) {
  const settled = auction.state === AuctionState.Settled;
  let grid: ReturnType<typeof buildAuctionGrid> | null = null;
  try {
    grid = buildAuctionGrid(auction);
  } catch {
    grid = null;
  }
  const largest = [
    ...auction.decryptedDemand,
    ...auction.decryptedSupply,
  ].reduce((max, value) => (value > max ? value : max), 0n);
  const width = (value: bigint) =>
    largest === 0n ? "0%" : `${Number((value * 1000n) / largest) / 10}%`;

  return (
    <div className="w-[210px]">
      <p className="font-mono text-xs">
        {settled
          ? auction.clearingVolume > 0n
            ? `${formatPrice(auction.clearingPrice)} · ${formatBase(auction.clearingVolume)} tBTC`
            : `${formatPrice(auction.referencePrice)} · no fill`
          : auction.state === AuctionState.Cancelled
            ? "Cancelled · refundable"
            : "Not published yet"}
      </p>
      {settled && grid !== null && largest > 0n ? (
        <div className="mt-1.5 flex flex-col">
          {[...grid].reverse().map((level) => {
            const demand = auction.decryptedDemand[level.index] ?? 0n;
            const supply = auction.decryptedSupply[level.index] ?? 0n;
            const clearing = level.index === auction.clearingIndex;
            return (
              <div
                key={level.index}
                className={`grid h-[11px] grid-cols-[minmax(0,1fr)_46px_minmax(0,1fr)] items-center gap-1 ${clearing ? "bg-accent/15" : ""}`}
              >
                <span className="flex justify-end">
                  <span
                    className="bg-buy h-[5px]"
                    style={{ width: width(demand) }}
                  />
                </span>
                <span
                  className={`text-center font-mono text-[9px] leading-none ${clearing ? "text-accent" : "text-faint"}`}
                >
                  {formatPrice(level.price)}
                </span>
                <span className="flex">
                  <span
                    className="bg-sell h-[5px]"
                    style={{ width: width(supply) }}
                  />
                </span>
              </div>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}
