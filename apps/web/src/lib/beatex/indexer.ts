import { AuctionState } from "@beatex/sdk";
import { z } from "zod";

import type { BatchSummary } from "@/features/auctions/model/batch-summary";

const integer = z
  .string()
  .regex(/^\d+$/)
  .transform((value) => BigInt(value));

const recordSchema = z.object({
  market: z.string(),
  auctionId: integer,
  outcome: z.enum(["settled", "cancelled", "empty"]),
  referencePrice: integer,
  referenceFromOracle: z.boolean(),
  gridStepBps: z.number().int().optional(),
  gridSize: z.number().int().optional(),
  clearingIndex: z.number().int().optional(),
  clearingPrice: integer.optional(),
  clearingVolume: integer.optional(),
  acceptedOrderCount: integer.optional(),
  demand: z.array(integer).optional(),
  supply: z.array(integer).optional(),
  slot: integer,
  signature: z.string(),
  blockTime: z.number().int().optional(),
});

/** One finished auction as served by the BeatEx indexer (`crates/indexer`). */
export type AuctionRecord = z.infer<typeof recordSchema>;

/**
 * Finished auctions for `market`, newest first. `before` pages to older auctions
 * (exclusive). The indexer caps `limit` at 500.
 */
export async function fetchAuctionHistory(
  market: string,
  options: { readonly limit?: number; readonly before?: bigint } = {},
  signal?: AbortSignal,
): Promise<readonly AuctionRecord[]> {
  const params = new URLSearchParams();
  if (options.limit !== undefined) params.set("limit", String(options.limit));
  if (options.before !== undefined)
    params.set("before", options.before.toString());
  const query = params.toString();
  const url = `/api/indexer/v1/markets/${encodeURIComponent(market)}/auctions${query ? `?${query}` : ""}`;
  // This public response is shared at the CDN; send no wallet/session cookies.
  const response = await fetch(url, { signal, credentials: "omit" });
  if (!response.ok) {
    throw new Error(`Indexer returned HTTP ${response.status}`);
  }
  return z.array(recordSchema).parse(await response.json());
}

/** Grid parameters to assume when a record omits them (empty auctions). */
export interface GridDefaults {
  readonly gridStepBps: number;
  readonly gridSize: number;
}

/**
 * Map an indexer record to the chart's batch summary. Empty auctions (no orders)
 * read as settled at their reference price with zero volume and empty curves.
 */
export function toBatchSummary(
  record: AuctionRecord,
  defaults: GridDefaults,
): BatchSummary {
  const gridSize = record.gridSize ?? defaults.gridSize;
  const zeros = Array.from({ length: gridSize }, () => 0n);
  return {
    auctionId: record.auctionId,
    state:
      record.outcome === "cancelled"
        ? AuctionState.Cancelled
        : AuctionState.Settled,
    referencePrice: record.referencePrice,
    referenceFromOracle: record.referenceFromOracle,
    gridStepBps: record.gridStepBps ?? defaults.gridStepBps,
    gridSize,
    clearingIndex: record.clearingIndex ?? (gridSize - 1) / 2,
    clearingPrice: record.clearingPrice ?? record.referencePrice,
    clearingVolume: record.clearingVolume ?? 0n,
    decryptedDemand: record.demand ?? zeros,
    decryptedSupply: record.supply ?? zeros,
    blockTime: record.blockTime ?? null,
  };
}

/** The newest `limit` finished auctions for `market`, as batch summaries. */
export async function fetchIndexedAuctions(
  market: string,
  defaults: GridDefaults,
  limit: number,
  signal?: AbortSignal,
): Promise<BatchSummary[]> {
  const records = await fetchAuctionHistory(market, { limit }, signal);
  return records.map((record) => toBatchSummary(record, defaults));
}
