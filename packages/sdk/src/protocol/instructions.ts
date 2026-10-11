import { BorshWriter, fixedBytes } from "./bytes.js";
import {
  CIPHERTEXT_BYTES,
  DIGEST_BYTES,
  ORDER_CIPHERTEXT_BYTES,
  type QueueCircuitKind,
} from "./constants.js";

export interface WireGroth16Proof {
  proofA: Uint8Array;
  proofB: Uint8Array;
  proofC: Uint8Array;
  publicInput: Uint8Array;
}

export interface BalanceTransitionArgs {
  baseAmount: bigint;
  quoteAmount: bigint;
  newEncryptedBase: Uint8Array;
  newEncryptedQuote: Uint8Array;
  proof: WireGroth16Proof;
}

export interface PublicWithdrawalArgs {
  baseAmount: bigint;
  quoteAmount: bigint;
  remainingEncryptedBase: Uint8Array;
  remainingEncryptedQuote: Uint8Array;
  proof: WireGroth16Proof;
}

export interface SubmitQueuedOrderArgs {
  clientOrderNonce: bigint;
  orderCommitment: Uint8Array;
  ciphertexts: Uint8Array;
  newEncryptedBase: Uint8Array;
  newEncryptedQuote: Uint8Array;
  proof: WireGroth16Proof;
}

export interface ReleaseOrderCollateralArgs {
  releasedEncryptedBase: Uint8Array;
  releasedEncryptedQuote: Uint8Array;
  proof: WireGroth16Proof;
}

export interface CancelActiveOrderArgs extends ReleaseOrderCollateralArgs {
  clientOrderNonce: bigint;
}

export interface ReplaceQueuedOrderArgs {
  release: ReleaseOrderCollateralArgs;
  replacement: SubmitQueuedOrderArgs;
}

export interface RolloverSettlementArgs {
  terminalNullifier: Uint8Array;
  creditedEncryptedBase: Uint8Array;
  creditedEncryptedQuote: Uint8Array;
}

export interface RolloverNextOrderArgs {
  clientOrderNonce: bigint;
  orderCommitment: Uint8Array;
  ciphertexts: Uint8Array;
  newEncryptedBase: Uint8Array;
  newEncryptedQuote: Uint8Array;
}

export const INSTRUCTION_DISCRIMINATORS = {
  initializeAuction: Uint8Array.of(37, 10, 117, 197, 208, 88, 117, 62),
  initializePrivateBalance: Uint8Array.of(168, 131, 243, 118, 163, 78, 34, 245),
  depositCollateral: Uint8Array.of(156, 131, 142, 116, 146, 247, 162, 120),
  withdrawPublicBalance: Uint8Array.of(93, 127, 192, 35, 215, 73, 145, 37),
  submitQueuedOrder: Uint8Array.of(128, 99, 67, 163, 40, 92, 175, 174),
  replaceQueuedOrder: Uint8Array.of(100, 123, 56, 163, 84, 124, 117, 228),
  cancelActiveOrder: Uint8Array.of(142, 213, 32, 174, 28, 253, 184, 204),
  verifyRolloverProof: Uint8Array.of(61, 51, 24, 207, 115, 154, 254, 28),
  bindRolloverSettlement: Uint8Array.of(23, 162, 178, 25, 112, 247, 88, 0),
  settleOrderToBalance: Uint8Array.of(182, 169, 109, 105, 203, 137, 244, 93),
  refundOrderToBalance: Uint8Array.of(179, 108, 74, 171, 210, 21, 114, 109),
  bindRolloverOrder: Uint8Array.of(32, 121, 37, 107, 128, 137, 144, 204),
  prepareRollover: Uint8Array.of(54, 228, 107, 138, 13, 136, 207, 109),
  rolloverOrder: Uint8Array.of(84, 248, 154, 20, 227, 55, 48, 237),
  closeOrderEvents: Uint8Array.of(244, 0, 216, 45, 166, 209, 29, 38),
} as const;

type InstructionName = keyof typeof INSTRUCTION_DISCRIMINATORS;

function instruction(name: InstructionName, body?: BorshWriter): Uint8Array {
  return new BorshWriter()
    .append(INSTRUCTION_DISCRIMINATORS[name])
    .append(body?.finish() ?? new Uint8Array())
    .finish();
}

function writeProof(writer: BorshWriter, proof: WireGroth16Proof): void {
  writer
    .fixed(proof.proofA, 64, "proofA")
    .fixed(proof.proofB, 128, "proofB")
    .fixed(proof.proofC, 64, "proofC")
    .fixed(proof.publicInput, 32, "publicInput");
}

function writeRelease(
  writer: BorshWriter,
  args: ReleaseOrderCollateralArgs,
): void {
  writer
    .fixed(args.releasedEncryptedBase, CIPHERTEXT_BYTES, "releasedEncryptedBase")
    .fixed(args.releasedEncryptedQuote, CIPHERTEXT_BYTES, "releasedEncryptedQuote");
  writeProof(writer, args.proof);
}

function writeSubmission(
  writer: BorshWriter,
  args: SubmitQueuedOrderArgs,
): void {
  writer
    .u64(args.clientOrderNonce)
    .fixed(args.orderCommitment, DIGEST_BYTES, "orderCommitment")
    .bytes(fixedBytes(args.ciphertexts, ORDER_CIPHERTEXT_BYTES, "ciphertexts"))
    .fixed(args.newEncryptedBase, CIPHERTEXT_BYTES, "newEncryptedBase")
    .fixed(args.newEncryptedQuote, CIPHERTEXT_BYTES, "newEncryptedQuote");
  writeProof(writer, args.proof);
}

function writeRolloverSettlement(
  writer: BorshWriter,
  args: RolloverSettlementArgs,
): void {
  writer
    .fixed(args.terminalNullifier, DIGEST_BYTES, "terminalNullifier")
    .fixed(args.creditedEncryptedBase, CIPHERTEXT_BYTES, "creditedEncryptedBase")
    .fixed(args.creditedEncryptedQuote, CIPHERTEXT_BYTES, "creditedEncryptedQuote");
}

function writeRolloverNextOrder(
  writer: BorshWriter,
  args: RolloverNextOrderArgs,
): void {
  writer
    .u64(args.clientOrderNonce)
    .fixed(args.orderCommitment, DIGEST_BYTES, "orderCommitment")
    .bytes(fixedBytes(args.ciphertexts, ORDER_CIPHERTEXT_BYTES, "ciphertexts"))
    .fixed(args.newEncryptedBase, CIPHERTEXT_BYTES, "newEncryptedBase")
    .fixed(args.newEncryptedQuote, CIPHERTEXT_BYTES, "newEncryptedQuote");
}

export function encodeInitializeAuction(auctionId: bigint): Uint8Array {
  return instruction("initializeAuction", new BorshWriter().u64(auctionId));
}

export function encodeInitializePrivateBalance(
  encryptedBase: Uint8Array,
  encryptedQuote: Uint8Array,
): Uint8Array {
  return instruction(
    "initializePrivateBalance",
    new BorshWriter()
      .fixed(encryptedBase, CIPHERTEXT_BYTES, "encryptedBase")
      .fixed(encryptedQuote, CIPHERTEXT_BYTES, "encryptedQuote"),
  );
}

export function encodeDepositCollateral(args: BalanceTransitionArgs): Uint8Array {
  const writer = new BorshWriter()
    .u64(args.baseAmount)
    .u64(args.quoteAmount)
    .fixed(args.newEncryptedBase, CIPHERTEXT_BYTES, "newEncryptedBase")
    .fixed(args.newEncryptedQuote, CIPHERTEXT_BYTES, "newEncryptedQuote");
  writeProof(writer, args.proof);
  return instruction("depositCollateral", writer);
}

export function encodeWithdrawPublicBalance(args: PublicWithdrawalArgs): Uint8Array {
  const writer = new BorshWriter()
    .u64(args.baseAmount)
    .u64(args.quoteAmount)
    .fixed(args.remainingEncryptedBase, CIPHERTEXT_BYTES, "remainingEncryptedBase")
    .fixed(args.remainingEncryptedQuote, CIPHERTEXT_BYTES, "remainingEncryptedQuote");
  writeProof(writer, args.proof);
  return instruction("withdrawPublicBalance", writer);
}

export function encodeSubmitQueuedOrder(args: SubmitQueuedOrderArgs): Uint8Array {
  const writer = new BorshWriter();
  writeSubmission(writer, args);
  return instruction("submitQueuedOrder", writer);
}

export function encodeReplaceQueuedOrder(args: ReplaceQueuedOrderArgs): Uint8Array {
  const writer = new BorshWriter();
  writeRelease(writer, args.release);
  writeSubmission(writer, args.replacement);
  return instruction("replaceQueuedOrder", writer);
}

export function encodeCancelActiveOrder(args: CancelActiveOrderArgs): Uint8Array {
  const writer = new BorshWriter().u64(args.clientOrderNonce);
  writeRelease(writer, args);
  return instruction("cancelActiveOrder", writer);
}

export function encodeRefundOrderToBalance(args: ReleaseOrderCollateralArgs): Uint8Array {
  const writer = new BorshWriter();
  writeRelease(writer, args);
  return instruction("refundOrderToBalance", writer);
}

export function encodeVerifyRolloverProof(args: {
  circuitKind: QueueCircuitKind;
  expectedPublicInput: Uint8Array;
  proof: WireGroth16Proof;
}): Uint8Array {
  const writer = new BorshWriter()
    .u8(args.circuitKind)
    .fixed(args.expectedPublicInput, DIGEST_BYTES, "expectedPublicInput");
  writeProof(writer, args.proof);
  return instruction("verifyRolloverProof", writer);
}

export function encodeBindRolloverSettlement(args: {
  settlement: RolloverSettlementArgs;
  expectedPublicInput: Uint8Array;
}): Uint8Array {
  const writer = new BorshWriter();
  writeRolloverSettlement(writer, args.settlement);
  writer.fixed(args.expectedPublicInput, DIGEST_BYTES, "expectedPublicInput");
  return instruction("bindRolloverSettlement", writer);
}

export function encodeSettleOrderToBalance(): Uint8Array {
  return instruction("settleOrderToBalance");
}

export function encodeCloseOrderEvents(): Uint8Array {
  return instruction("closeOrderEvents");
}

export function encodeBindRolloverOrder(args: {
  nextOrder: RolloverNextOrderArgs;
  creditedEncryptedBase: Uint8Array;
  creditedEncryptedQuote: Uint8Array;
  expectedPublicInput: Uint8Array;
}): Uint8Array {
  const writer = new BorshWriter();
  writeRolloverNextOrder(writer, args.nextOrder);
  writer
    .fixed(args.creditedEncryptedBase, CIPHERTEXT_BYTES, "creditedEncryptedBase")
    .fixed(args.creditedEncryptedQuote, CIPHERTEXT_BYTES, "creditedEncryptedQuote")
    .fixed(args.expectedPublicInput, DIGEST_BYTES, "expectedPublicInput");
  return instruction("bindRolloverOrder", writer);
}

export function encodePrepareRollover(): Uint8Array {
  return instruction("prepareRollover");
}

export function encodeRolloverOrder(): Uint8Array {
  return instruction("rolloverOrder");
}
