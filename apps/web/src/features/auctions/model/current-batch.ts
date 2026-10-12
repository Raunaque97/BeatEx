import {
  auctionIdAtSlot,
  auctionWindow,
  buildAuctionGrid,
  deriveAuctionPhase,
  previewReferencePrice,
  type AuctionAccount,
  type AuctionGridLevel,
  type AuctionPhase,
  type AuctionWindow,
  type MarketAccount,
  type PythPriceUpdate,
} from "@beatex/sdk";

export type ReferenceSource = "previous-clear" | "oracle";

export interface CurrentBatch {
  readonly auctionId: bigint;
  readonly window: AuctionWindow;
  readonly phase: AuctionPhase;
  readonly slotsLeft: bigint;
  /** Fraction of the collection window elapsed, 0..1. */
  readonly progress: number;
  readonly auction: AuctionAccount | null;
  /** Exact grid from the Auction account, or a preview of what initialization would snapshot. */
  readonly grid: readonly AuctionGridLevel[] | null;
  readonly referencePrice: bigint | null;
  readonly referenceSource: ReferenceSource | null;
  /** True while the grid is a preview because no Auction account exists yet. */
  readonly gridIsPreview: boolean;
}

export function describeCurrentBatch(
  market: MarketAccount,
  slot: bigint,
  auction: AuctionAccount | null,
  oracle: PythPriceUpdate | null,
): CurrentBatch | null {
  const auctionId = auctionIdAtSlot(market, slot);
  if (auctionId === null) return null;
  const window = auctionWindow(market, auctionId);
  const elapsed = Number(slot - window.openSlot);
  const duration = Number(market.submissionDurationSlots);
  const slotsLeft = window.submissionEndSlot - slot;

  let referencePrice: bigint | null = null;
  let referenceSource: ReferenceSource | null = null;
  let grid: readonly AuctionGridLevel[] | null = null;
  if (auction !== null) {
    referencePrice = auction.referencePrice;
    referenceSource = auction.referenceFromOracle ? "oracle" : "previous-clear";
    grid = safeGrid(auction);
  } else {
    const preview = previewReferencePrice(market, auctionId, oracle);
    if (preview !== null) {
      referencePrice = preview.referencePrice;
      referenceSource = preview.fromOracle ? "oracle" : "previous-clear";
      grid = safeGrid({
        referencePrice: preview.referencePrice,
        gridStepBps: market.gridStepBps,
        gridSize: market.gridRadius * 2 + 1,
      });
    }
  }

  return {
    auctionId,
    window,
    phase: deriveAuctionPhase(auction, slot),
    slotsLeft: slotsLeft > 0n ? slotsLeft : 0n,
    progress: Math.min(1, Math.max(0, elapsed / duration)),
    auction,
    grid,
    referencePrice,
    referenceSource,
    gridIsPreview: auction === null && grid !== null,
  };
}

function safeGrid(
  auction: Pick<AuctionAccount, "referencePrice" | "gridStepBps" | "gridSize">,
): readonly AuctionGridLevel[] | null {
  try {
    return buildAuctionGrid(auction);
  } catch {
    return null;
  }
}
