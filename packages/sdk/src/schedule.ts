import { AuctionState } from "./protocol/constants.js";
import type { AuctionAccount, MarketAccount } from "./protocol/accounts.js";
import { normalizeOraclePrice, type PythPriceUpdate } from "./protocol/oracle.js";

export type AuctionPhase =
  | "not-started"
  | "collecting"
  | "aggregating"
  | "publishing"
  | "settled"
  | "cancelled"
  | "expired";

export interface AuctionWindow {
  readonly auctionId: bigint;
  readonly openSlot: bigint;
  readonly submissionEndSlot: bigint;
  readonly aggregationDeadlineSlot: bigint;
  readonly decryptionDeadlineSlot: bigint;
}

type MarketSchedule = Pick<
  MarketAccount,
  "marketStartSlot" | "submissionDurationSlots" | "aggregationDurationSlots" | "decryptionDurationSlots"
>;

/** The slot-derived auction ID, or null before the market start slot. */
export function auctionIdAtSlot(market: MarketSchedule, slot: bigint): bigint | null {
  if (slot < market.marketStartSlot) return null;
  return (slot - market.marketStartSlot) / market.submissionDurationSlots;
}

/** The deadlines `initialize_auction` would snapshot for `auctionId`. */
export function auctionWindow(market: MarketSchedule, auctionId: bigint): AuctionWindow {
  const openSlot = market.marketStartSlot + auctionId * market.submissionDurationSlots;
  const submissionEndSlot = openSlot + market.submissionDurationSlots;
  const aggregationDeadlineSlot = submissionEndSlot + market.aggregationDurationSlots;
  const decryptionDeadlineSlot = aggregationDeadlineSlot + market.decryptionDurationSlots;
  return { auctionId, openSlot, submissionEndSlot, aggregationDeadlineSlot, decryptionDeadlineSlot };
}

/** User-facing phase from the stored state, the auction deadlines, and the current slot. */
export function deriveAuctionPhase(
  auction: Pick<
    AuctionAccount,
    "state" | "submissionEndSlot" | "aggregationDeadlineSlot" | "decryptionDeadlineSlot"
  > | null,
  slot: bigint,
): AuctionPhase {
  if (auction === null) return "not-started";
  if (auction.state === AuctionState.Settled) return "settled";
  if (auction.state === AuctionState.Cancelled) return "cancelled";
  if (slot < auction.submissionEndSlot) return "collecting";
  if (slot < auction.aggregationDeadlineSlot) return "aggregating";
  if (slot < auction.decryptionDeadlineSlot) return "publishing";
  return "expired";
}

export interface ReferencePricePreview {
  readonly referencePrice: bigint;
  readonly fromOracle: boolean;
}

/**
 * The reference price `initialize_auction` would choose for `auctionId` right now: the
 * clearing price of auction `auctionId - 2` (or later) when it is eligible, otherwise the normalized Pyth price.
 * Only for display before the auction exists; an initialized Auction account is authoritative.
 */
export function previewReferencePrice(
  market: Pick<
    MarketAccount,
    | "lastClearedAuctionSequence"
    | "lastClearingVolume"
    | "lastClearingPrice"
    | "minimumReferenceVolume"
    | "oracleExponentAdjustment"
  >,
  auctionId: bigint,
  oracle: Pick<PythPriceUpdate, "price" | "exponent"> | null,
): ReferencePricePreview | null {
  // The sequence stores the cleared auction's id plus one; auction N+1 may use N-1's price.
  const usePrevious =
    auctionId > 0n &&
    market.lastClearedAuctionSequence > 0n &&
    market.lastClearedAuctionSequence + 1n >= auctionId &&
    market.lastClearingVolume >= market.minimumReferenceVolume;
  if (usePrevious) return { referencePrice: market.lastClearingPrice, fromOracle: false };
  if (oracle === null || oracle.price <= 0n) return null;
  return {
    referencePrice: normalizeOraclePrice(oracle.price, oracle.exponent, market.oracleExponentAdjustment),
    fromOracle: true,
  };
}
