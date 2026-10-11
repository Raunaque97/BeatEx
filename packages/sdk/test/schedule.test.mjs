import assert from "node:assert/strict";
import test from "node:test";
import {
  ACCOUNT_DISCRIMINATORS,
  AUCTION_ACCOUNT_LAYOUT,
  AuctionState,
  PYTH_PRICE_UPDATE_V2_DISCRIMINATOR,
  auctionIdAtSlot,
  auctionWindow,
  decodePythPriceUpdate,
  deriveAuctionPhase,
  normalizeOraclePrice,
  previewReferencePrice,
} from "../dist/index.js";

const market = {
  marketStartSlot: 1_000n,
  submissionDurationSlots: 20n,
  aggregationDurationSlots: 200n,
  decryptionDurationSlots: 200n,
  lastClearedAuctionSequence: 5n,
  lastClearingVolume: 3n,
  lastClearingPrice: 85_500n,
  minimumReferenceVolume: 1n,
  oracleExponentAdjustment: 0,
};

test("auction IDs and windows follow the market schedule", () => {
  assert.equal(auctionIdAtSlot(market, 999n), null);
  assert.equal(auctionIdAtSlot(market, 1_000n), 0n);
  assert.equal(auctionIdAtSlot(market, 1_119n), 5n);
  assert.deepEqual(auctionWindow(market, 5n), {
    auctionId: 5n,
    openSlot: 1_100n,
    submissionEndSlot: 1_120n,
    aggregationDeadlineSlot: 1_320n,
    decryptionDeadlineSlot: 1_520n,
  });
});

test("phases are derived from state and deadlines", () => {
  const pending = { state: AuctionState.Pending, ...auctionWindow(market, 5n) };
  assert.equal(deriveAuctionPhase(null, 1_110n), "not-started");
  assert.equal(deriveAuctionPhase(pending, 1_119n), "collecting");
  assert.equal(deriveAuctionPhase(pending, 1_120n), "aggregating");
  assert.equal(deriveAuctionPhase(pending, 1_320n), "publishing");
  assert.equal(deriveAuctionPhase(pending, 1_520n), "expired");
  assert.equal(deriveAuctionPhase({ ...pending, state: AuctionState.Settled }, 1_110n), "settled");
  assert.equal(deriveAuctionPhase({ ...pending, state: AuctionState.Cancelled }, 1_110n), "cancelled");
});

test("oracle normalization matches the program's half-up rounding", () => {
  assert.equal(normalizeOraclePrice(8_550_049_999_999n, -8, 0), 85_500n);
  assert.equal(normalizeOraclePrice(8_550_050_000_000n, -8, 0), 85_501n);
  assert.equal(normalizeOraclePrice(855n, 2, 0), 85_500n);
  assert.throws(() => normalizeOraclePrice(0n, -8, 0));
});

test("reference preview mirrors initialize_auction's choice", () => {
  const oracle = { price: 8_296_400_000_000n, exponent: -8 };
  assert.deepEqual(previewReferencePrice(market, 5n, oracle), { referencePrice: 85_500n, fromOracle: false });
  assert.deepEqual(previewReferencePrice(market, 6n, oracle), { referencePrice: 85_500n, fromOracle: false });
  assert.deepEqual(previewReferencePrice(market, 7n, oracle), { referencePrice: 82_964n, fromOracle: true });
  assert.deepEqual(previewReferencePrice({ ...market, lastClearingVolume: 0n }, 5n, oracle), {
    referencePrice: 82_964n,
    fromOracle: true,
  });
  assert.equal(previewReferencePrice(market, 7n, null), null);
});

test("Pyth PriceUpdateV2 accounts decode for both verification levels", () => {
  const body = (level) => {
    const parts = [PYTH_PRICE_UPDATE_V2_DISCRIMINATOR, new Uint8Array(32), level];
    const message = new Uint8Array(32 + 8 + 8 + 4 + 8 + 8 + 8 + 8 + 8);
    message.fill(7, 0, 32);
    const view = new DataView(message.buffer);
    view.setBigInt64(32, 8_296_400_000_000n, true);
    view.setBigUint64(40, 1_000_000n, true);
    view.setInt32(48, -8, true);
    view.setBigInt64(52, 1_760_000_000n, true);
    view.setBigUint64(84, 99n, true);
    return Uint8Array.from([...parts.flatMap((part) => [...part]), ...message]);
  };
  for (const level of [Uint8Array.of(1), Uint8Array.of(0, 5)]) {
    const decoded = decodePythPriceUpdate(body(level));
    assert.equal(decoded.price, 8_296_400_000_000n);
    assert.equal(decoded.conf, 1_000_000n);
    assert.equal(decoded.exponent, -8);
    assert.equal(decoded.publishTime, 1_760_000_000n);
    assert.equal(decoded.postedSlot, 99n);
    assert.equal(decoded.feedId[0], 7);
  }
  assert.throws(() => decodePythPriceUpdate(new Uint8Array(200)));
});

test("auction layout constants match the decoder", () => {
  assert.equal(ACCOUNT_DISCRIMINATORS.auction.length, 8);
  assert.equal(AUCTION_ACCOUNT_LAYOUT.size, 2642);
});
