import {
  getAddressEncoder,
  getProgramDerivedAddress,
  type Address,
  type ProgramDerivedAddress,
} from "@solana/kit";
import { u16Bytes, u64Bytes } from "./bytes.js";
import {
  BEATEX_PROGRAM_ADDRESS,
  type QueueCircuitKind,
} from "./constants.js";

const addressEncoder = getAddressEncoder();

export interface MarketSeeds {
  baseMint: Address;
  quoteMint: Address;
  marketNonce: bigint;
}

export function deriveMarketPda(
  seeds: MarketSeeds,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: [
      "market",
      addressEncoder.encode(seeds.baseMint),
      addressEncoder.encode(seeds.quoteMint),
      u64Bytes(seeds.marketNonce),
    ],
  });
}

export function deriveVaultAuthorityPda(
  market: Address,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: ["vault-authority", addressEncoder.encode(market)],
  });
}

export function deriveKeyEpochPda(
  market: Address,
  epoch: bigint,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: ["key-epoch", addressEncoder.encode(market), u64Bytes(epoch)],
  });
}

export function deriveAuctionPda(
  market: Address,
  auctionId: bigint,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: ["auction", addressEncoder.encode(market), u64Bytes(auctionId)],
  });
}

export function deriveSubmissionCounterPda(
  auction: Address,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: ["submission-counter", addressEncoder.encode(auction)],
  });
}

export function deriveAggregateAccumulatorPda(
  auction: Address,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: ["aggregate-accumulator", addressEncoder.encode(auction)],
  });
}

export function derivePrivateBalancePda(
  market: Address,
  owner: Address,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: [
      "private-balance",
      addressEncoder.encode(market),
      addressEncoder.encode(owner),
    ],
  });
}

export function deriveVerificationKeyPda(
  market: Address,
  circuitKind: QueueCircuitKind,
  circuitVersion: number,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: [
      "queue-vk",
      addressEncoder.encode(market),
      Uint8Array.of(circuitKind),
      u16Bytes(circuitVersion),
    ],
  });
}

export function deriveQueuedOrderPda(
  auction: Address,
  owner: Address,
  clientOrderNonce: bigint,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: [
      "queued-order",
      addressEncoder.encode(auction),
      addressEncoder.encode(owner),
      u64Bytes(clientOrderNonce),
    ],
  });
}

export function deriveRolloverOrderPda(
  auction: Address,
  owner: Address,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: [
      "rollover-order",
      addressEncoder.encode(auction),
      addressEncoder.encode(owner),
    ],
  });
}

export function deriveActiveOrderPda(
  auction: Address,
  owner: Address,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<ProgramDerivedAddress> {
  return getProgramDerivedAddress({
    programAddress,
    seeds: [
      "active-order",
      addressEncoder.encode(auction),
      addressEncoder.encode(owner),
    ],
  });
}

export interface AuctionPdas {
  auctionId: bigint;
  auction: ProgramDerivedAddress;
  submissionCounter: ProgramDerivedAddress;
  accumulator: ProgramDerivedAddress;
}

export async function deriveAuctionPdas(
  market: Address,
  auctionId: bigint,
  programAddress: Address = BEATEX_PROGRAM_ADDRESS,
): Promise<AuctionPdas> {
  const auction = await deriveAuctionPda(market, auctionId, programAddress);
  const [submissionCounter, accumulator] = await Promise.all([
    deriveSubmissionCounterPda(auction[0], programAddress),
    deriveAggregateAccumulatorPda(auction[0], programAddress),
  ]);
  return { auctionId, auction, submissionCounter, accumulator };
}
