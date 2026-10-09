import type { Address } from "@solana/kit";
import { BorshReader, bytesEqual } from "./bytes.js";
import {
  AuctionState,
  CIPHERTEXT_BYTES,
  DIGEST_BYTES,
  PRICE_LEVELS,
  QueuedOrderState,
  QueueCircuitKind,
  WithdrawalState,
} from "./constants.js";

export const ACCOUNT_DISCRIMINATORS = {
  market: Uint8Array.of(219, 190, 213, 55, 0, 227, 198, 154),
  auction: Uint8Array.of(218, 94, 247, 242, 126, 233, 131, 81),
  queueVerificationKey: Uint8Array.of(187, 29, 111, 248, 250, 134, 144, 100),
  queuedOrder: Uint8Array.of(35, 99, 163, 153, 175, 232, 22, 207),
  activeOrder: Uint8Array.of(141, 51, 124, 142, 175, 61, 58, 58),
  submissionCounter: Uint8Array.of(111, 42, 197, 65, 236, 245, 22, 103),
  aggregateAccumulator: Uint8Array.of(180, 219, 77, 246, 185, 39, 119, 145),
  encryptedPrivateBalance: Uint8Array.of(59, 34, 210, 165, 229, 40, 41, 199),
  keyEpoch: Uint8Array.of(218, 80, 118, 150, 116, 199, 47, 171),
} as const;

function accountReader(
  data: Uint8Array,
  discriminator: Uint8Array,
  name: string,
): BorshReader {
  if (data.length < 8 || !bytesEqual(data.subarray(0, 8), discriminator)) {
    throw new Error(`invalid ${name} account discriminator`);
  }
  return new BorshReader(data.subarray(8));
}

function fixedArray(
  count: number,
  read: () => Uint8Array,
): readonly Uint8Array[] {
  return Array.from({ length: count }, read);
}

function u64Array(reader: BorshReader, count: number): readonly bigint[] {
  return Array.from({ length: count }, () => reader.u64());
}

export interface MarketAccount {
  version: number;
  authority: Address;
  baseMint: Address;
  quoteMint: Address;
  baseVault: Address;
  quoteVault: Address;
  baseUnitSize: bigint;
  quoteUnitSize: bigint;
  gridStepBps: number;
  gridRadius: number;
  maxOrderQuantity: bigint;
  maxAggregateQuantity: bigint;
  currentKeyEpoch: Address;
  marketStartSlot: bigint;
  lastClearingPrice: bigint;
  lastClearingVolume: bigint;
  lastClearedAuctionSequence: bigint;
  minimumReferenceVolume: bigint;
  oraclePriceAccount: Address;
  oracleFeedId: Uint8Array;
  oracleMaxAgeSeconds: bigint;
  oracleMaxConfidenceBps: number;
  oracleExponentAdjustment: number;
  minReferencePrice: bigint;
  maxReferencePrice: bigint;
  submissionDurationSlots: bigint;
  aggregationDurationSlots: bigint;
  decryptionDurationSlots: bigint;
  allocationRuleVersion: number;
  orderCircuitVersion: number;
  claimCircuitVersion: number;
  paused: boolean;
  vaultAuthorityBump: number;
  bump: number;
}

export function decodeMarket(data: Uint8Array): MarketAccount {
  const reader = accountReader(data, ACCOUNT_DISCRIMINATORS.market, "Market");
  const version = reader.u8();
  const authority = reader.address();
  const baseMint = reader.address();
  const quoteMint = reader.address();
  const baseVault = reader.address();
  const quoteVault = reader.address();
  const baseUnitSize = reader.u64();
  const quoteUnitSize = reader.u64();
  const gridStepBps = reader.u16();
  const gridRadius = reader.u8();
  const maxOrderQuantity = reader.u64();
  const maxAggregateQuantity = reader.u64();
  const currentKeyEpoch = reader.address();
  const marketStartSlot = reader.u64();
  const lastClearingPrice = reader.u64();
  const lastClearingVolume = reader.u64();
  const lastClearedAuctionSequence = reader.u64();
  const minimumReferenceVolume = reader.u64();
  const oraclePriceAccount = reader.address();
  const oracleFeedId = reader.fixed(DIGEST_BYTES, "oracleFeedId");
  const oracleMaxAgeSeconds = reader.u64();
  const oracleMaxConfidenceBps = reader.u16();
  const exponentByte = reader.u8();
  const oracleExponentAdjustment = exponentByte > 127 ? exponentByte - 256 : exponentByte;
  const minReferencePrice = reader.u64();
  const maxReferencePrice = reader.u64();
  const submissionDurationSlots = reader.u64();
  const aggregationDurationSlots = reader.u64();
  const decryptionDurationSlots = reader.u64();
  const allocationRuleVersion = reader.u16();
  const orderCircuitVersion = reader.u16();
  const claimCircuitVersion = reader.u16();
  const paused = reader.bool();
  const vaultAuthorityBump = reader.u8();
  const bump = reader.u8();
  return {
    version,
    authority,
    baseMint,
    quoteMint,
    baseVault,
    quoteVault,
    baseUnitSize,
    quoteUnitSize,
    gridStepBps,
    gridRadius,
    maxOrderQuantity,
    maxAggregateQuantity,
    currentKeyEpoch,
    marketStartSlot,
    lastClearingPrice,
    lastClearingVolume,
    lastClearedAuctionSequence,
    minimumReferenceVolume,
    oraclePriceAccount,
    oracleFeedId,
    oracleMaxAgeSeconds,
    oracleMaxConfidenceBps,
    oracleExponentAdjustment,
    minReferencePrice,
    maxReferencePrice,
    submissionDurationSlots,
    aggregationDurationSlots,
    decryptionDurationSlots,
    allocationRuleVersion,
    orderCircuitVersion,
    claimCircuitVersion,
    paused,
    vaultAuthorityBump,
    bump,
  };
}

export interface AuctionAccount {
  version: number;
  market: Address;
  auctionId: bigint;
  keyEpoch: Address;
  state: AuctionState;
  openSlot: bigint;
  submissionEndSlot: bigint;
  aggregationDeadlineSlot: bigint;
  decryptionDeadlineSlot: bigint;
  referencePrice: bigint;
  referenceFromOracle: boolean;
  oraclePublishTime: bigint;
  gridStepBps: number;
  gridSize: number;
  allocationRuleVersion: number;
  orderCircuitVersion: number;
  claimCircuitVersion: number;
  finalizedOrderCount: number;
  aggregateBuy: readonly Uint8Array[];
  aggregateSell: readonly Uint8Array[];
  decryptedDemand: readonly bigint[];
  decryptedSupply: readonly bigint[];
  decryptedEntryBitmap: bigint;
  clearingIndex: number;
  clearingPrice: bigint;
  clearingVolume: bigint;
  baseResidual: bigint;
  quoteResidual: bigint;
  bump: number;
}

export function decodeAuction(data: Uint8Array): AuctionAccount {
  const reader = accountReader(data, ACCOUNT_DISCRIMINATORS.auction, "Auction");
  const version = reader.u8();
  const market = reader.address();
  const auctionId = reader.u64();
  const keyEpoch = reader.address();
  const state = reader.u8() as AuctionState;
  const openSlot = reader.u64();
  const submissionEndSlot = reader.u64();
  const aggregationDeadlineSlot = reader.u64();
  const decryptionDeadlineSlot = reader.u64();
  const referencePrice = reader.u64();
  const referenceFromOracle = reader.bool();
  const oraclePublishTime = reader.i64();
  const gridStepBps = reader.u16();
  const gridSize = reader.u8();
  const allocationRuleVersion = reader.u16();
  const orderCircuitVersion = reader.u16();
  const claimCircuitVersion = reader.u16();
  const finalizedOrderCount = reader.u32();
  const aggregateBuy = fixedArray(PRICE_LEVELS, () =>
    reader.fixed(CIPHERTEXT_BYTES, "aggregateBuy"),
  );
  const aggregateSell = fixedArray(PRICE_LEVELS, () =>
    reader.fixed(CIPHERTEXT_BYTES, "aggregateSell"),
  );
  const decryptedDemand = u64Array(reader, PRICE_LEVELS);
  const decryptedSupply = u64Array(reader, PRICE_LEVELS);
  const decryptedEntryBitmap = reader.u128();
  const clearingIndex = reader.u8();
  const clearingPrice = reader.u64();
  const clearingVolume = reader.u64();
  const baseResidual = reader.u64();
  const quoteResidual = reader.u64();
  const bump = reader.u8();
  return {
    version,
    market,
    auctionId,
    keyEpoch,
    state,
    openSlot,
    submissionEndSlot,
    aggregationDeadlineSlot,
    decryptionDeadlineSlot,
    referencePrice,
    referenceFromOracle,
    oraclePublishTime,
    gridStepBps,
    gridSize,
    allocationRuleVersion,
    orderCircuitVersion,
    claimCircuitVersion,
    finalizedOrderCount,
    aggregateBuy,
    aggregateSell,
    decryptedDemand,
    decryptedSupply,
    decryptedEntryBitmap,
    clearingIndex,
    clearingPrice,
    clearingVolume,
    baseResidual,
    quoteResidual,
    bump,
  };
}

export interface EncryptedPrivateBalanceAccount {
  version: number;
  market: Address;
  owner: Address;
  keyEpoch: Address;
  encryptedBase: Uint8Array;
  encryptedQuote: Uint8Array;
  depositedBase: bigint;
  depositedQuote: bigint;
  balanceDigest: Uint8Array;
  revision: bigint;
  bump: number;
}

export function decodeEncryptedPrivateBalance(
  data: Uint8Array,
): EncryptedPrivateBalanceAccount {
  const reader = accountReader(
    data,
    ACCOUNT_DISCRIMINATORS.encryptedPrivateBalance,
    "EncryptedPrivateBalance",
  );
  return {
    version: reader.u8(),
    market: reader.address(),
    owner: reader.address(),
    keyEpoch: reader.address(),
    encryptedBase: reader.fixed(CIPHERTEXT_BYTES, "encryptedBase"),
    encryptedQuote: reader.fixed(CIPHERTEXT_BYTES, "encryptedQuote"),
    depositedBase: reader.u64(),
    depositedQuote: reader.u64(),
    balanceDigest: reader.fixed(DIGEST_BYTES, "balanceDigest"),
    revision: reader.u64(),
    bump: reader.u8(),
  };
}

export interface SubmissionCounterAccount {
  version: number;
  auction: Address;
  acceptedOrderCount: bigint;
  bump: number;
}

export function decodeSubmissionCounter(data: Uint8Array): SubmissionCounterAccount {
  const reader = accountReader(
    data,
    ACCOUNT_DISCRIMINATORS.submissionCounter,
    "SubmissionCounter",
  );
  return {
    version: reader.u8(),
    auction: reader.address(),
    acceptedOrderCount: reader.u64(),
    bump: reader.u8(),
  };
}

export interface ActiveOrderAccount {
  version: number;
  market: Address;
  auction: Address;
  owner: Address;
  latestEvent: Address;
  revision: bigint;
  active: boolean;
  bump: number;
}

export function decodeActiveOrder(data: Uint8Array): ActiveOrderAccount {
  const reader = accountReader(data, ACCOUNT_DISCRIMINATORS.activeOrder, "ActiveOrder");
  return {
    version: reader.u8(),
    market: reader.address(),
    auction: reader.address(),
    owner: reader.address(),
    latestEvent: reader.address(),
    revision: reader.u64(),
    active: reader.bool(),
    bump: reader.u8(),
  };
}

export interface QueuedOrderAccount {
  version: number;
  market: Address;
  auction: Address;
  owner: Address;
  funder: Address;
  clientOrderNonce: bigint;
  sequence: bigint;
  state: QueuedOrderState;
  withdrawalState: WithdrawalState;
  terminalNullifier: Uint8Array;
  previousEvent: Address;
  ciphertexts: Uint8Array;
  ciphertextDigest: Uint8Array;
  orderCommitment: Uint8Array;
  orderCircuitVersion: number;
  bump: number;
}

export function decodeQueuedOrder(data: Uint8Array): QueuedOrderAccount {
  const reader = accountReader(data, ACCOUNT_DISCRIMINATORS.queuedOrder, "QueuedOrder");
  return {
    version: reader.u8(),
    market: reader.address(),
    auction: reader.address(),
    owner: reader.address(),
    funder: reader.address(),
    clientOrderNonce: reader.u64(),
    sequence: reader.u64(),
    state: reader.u8() as QueuedOrderState,
    withdrawalState: reader.u8() as WithdrawalState,
    terminalNullifier: reader.fixed(DIGEST_BYTES, "terminalNullifier"),
    previousEvent: reader.address(),
    ciphertexts: reader.bytes("ciphertexts"),
    ciphertextDigest: reader.fixed(DIGEST_BYTES, "ciphertextDigest"),
    orderCommitment: reader.fixed(DIGEST_BYTES, "orderCommitment"),
    orderCircuitVersion: reader.u16(),
    bump: reader.u8(),
  };
}

export interface AggregateAccumulatorAccount {
  version: number;
  auction: Address;
  foldedOrderCount: bigint;
  foldRevision: bigint;
  ciphertexts: Uint8Array;
  ciphertextDigest: Uint8Array;
  bump: number;
}

export function decodeAggregateAccumulator(
  data: Uint8Array,
): AggregateAccumulatorAccount {
  const reader = accountReader(
    data,
    ACCOUNT_DISCRIMINATORS.aggregateAccumulator,
    "AggregateAccumulator",
  );
  return {
    version: reader.u8(),
    auction: reader.address(),
    foldedOrderCount: reader.u64(),
    foldRevision: reader.u64(),
    ciphertexts: reader.bytes("ciphertexts"),
    ciphertextDigest: reader.fixed(DIGEST_BYTES, "ciphertextDigest"),
    bump: reader.u8(),
  };
}

export interface QueueVerificationKeyAccount {
  version: number;
  market: Address;
  circuitKind: QueueCircuitKind;
  circuitVersion: number;
  alphaG1: Uint8Array;
  betaG2: Uint8Array;
  gammaG2: Uint8Array;
  deltaG2: Uint8Array;
  ic: readonly [Uint8Array, Uint8Array];
  bump: number;
}

export function decodeQueueVerificationKey(data: Uint8Array): QueueVerificationKeyAccount {
  const reader = accountReader(
    data,
    ACCOUNT_DISCRIMINATORS.queueVerificationKey,
    "QueueVerificationKey",
  );
  const version = reader.u8();
  const market = reader.address();
  const circuitKind = reader.u8() as QueueCircuitKind;
  const circuitVersion = reader.u16();
  const alphaG1 = reader.fixed(64, "alphaG1");
  const betaG2 = reader.fixed(128, "betaG2");
  const gammaG2 = reader.fixed(128, "gammaG2");
  const deltaG2 = reader.fixed(128, "deltaG2");
  const ic = [reader.fixed(64, "ic[0]"), reader.fixed(64, "ic[1]")] as const;
  return {
    version,
    market,
    circuitKind,
    circuitVersion,
    alphaG1,
    betaG2,
    gammaG2,
    deltaG2,
    ic,
    bump: reader.u8(),
  };
}

export interface DleqProof {
  commitment1: Uint8Array;
  commitment2: Uint8Array;
  response: Uint8Array;
}

export interface KeyEpochAccount {
  authority: Address;
  market: Address;
  epoch: bigint;
  encryptionPubkey: Uint8Array;
  babyjubEncryptionPubkey: Uint8Array;
  babyjubParticipantPublicShares: readonly Uint8Array[];
  coefficientCommitments: readonly Uint8Array[];
  participantPublicShares: readonly Uint8Array[];
  participantAuthorities: readonly Address[];
  keyRelationProof: DleqProof;
  threshold: number;
  proofTranscriptVersion: number;
  activationSlot: bigint;
  retired: boolean;
  acknowledgementBitmap: number;
  bump: number;
}

export function decodeKeyEpoch(data: Uint8Array): KeyEpochAccount {
  const reader = accountReader(data, ACCOUNT_DISCRIMINATORS.keyEpoch, "KeyEpoch");
  const authority = reader.address();
  const market = reader.address();
  const epoch = reader.u64();
  const encryptionPubkey = reader.fixed(32, "encryptionPubkey");
  const babyjubEncryptionPubkey = reader.fixed(32, "babyjubEncryptionPubkey");
  const babyjubParticipantPublicShares = fixedArray(7, () =>
    reader.fixed(32, "babyjubParticipantPublicShare"),
  );
  const coefficientCommitments = fixedArray(3, () =>
    reader.fixed(32, "coefficientCommitment"),
  );
  const participantPublicShares = fixedArray(7, () =>
    reader.fixed(32, "participantPublicShare"),
  );
  const participantAuthorities = Array.from({ length: 7 }, () => reader.address());
  const keyRelationProof = {
    commitment1: reader.fixed(32, "keyRelationProof.commitment1"),
    commitment2: reader.fixed(32, "keyRelationProof.commitment2"),
    response: reader.fixed(32, "keyRelationProof.response"),
  };
  return {
    authority,
    market,
    epoch,
    encryptionPubkey,
    babyjubEncryptionPubkey,
    babyjubParticipantPublicShares,
    coefficientCommitments,
    participantPublicShares,
    participantAuthorities,
    keyRelationProof,
    threshold: reader.u8(),
    proofTranscriptVersion: reader.u8(),
    activationSlot: reader.u64(),
    retired: reader.bool(),
    acknowledgementBitmap: reader.u8(),
    bump: reader.u8(),
  };
}
