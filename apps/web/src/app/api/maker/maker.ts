import { createHash } from "node:crypto";

import {
  AuctionState,
  BASE_UNIT_SIZE,
  InMemoryPrivateStateStorage,
  auctionIdAtSlot,
  auctionWindow,
  buildAuctionGrid,
  createBeatExClient,
  createBeatExRpc,
  createKitSignerAdapter,
  decodeMarket,
  decodePythPriceUpdate,
  derivePrivateBalancePda,
  hasOracle,
  isAuctionResult,
  parseDecimalUnits,
  previewReferencePrice,
  type AuctionGridLevel,
  type BeatExClient,
  type BeatExProver,
  type BeatExRpc,
  type MarketAccount,
  type OrderCurve,
} from "@beatex/sdk";
import {
  address,
  createKeyPairSignerFromBytes,
  createKeyPairSignerFromPrivateKeyBytes,
  type Address,
  type KeyPairSigner,
} from "@solana/kit";

import type { MarketConfig } from "@/lib/config/markets";
import { publicEnv } from "@/lib/config/public-env";
import { sendInstructions } from "@/lib/solana/send";
import {
  associatedTokenAddress,
  closeTokenAccount,
  createAssociatedTokenAccountIdempotent,
  createSystemAccount,
  mintToChecked,
  systemTransfer,
  tokenAccountAmount,
} from "@/lib/solana/token";

/** Stop waiting for a clear with time left to settle inside the function's limit. */
const SETTLE_DEADLINE_MS = 240_000;
const AUCTION_POLL_MS = 2_000;
const OPEN_POLL_MS = 200;
/** How early the program accepts the next auction with no recent clear. */
const OPEN_LEAD_SLOTS = 2n;
/** The fastest Devnet slot, so sleeping for a slot count never overshoots. */
const FASTEST_SLOT_MS = 200n;
const TRANSACTION_FEE_LAMPORTS = 5_000n;
/** Slots a maker needs to fund and deposit, even when Devnet's slots run fast. */
const MIN_PREPARATION_SLOTS = 60n;
/** Deposit headroom for the price moving before the auction snapshots it. */
const DEPOSIT_MARGIN_PERCENT = 110n;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

export class MakerError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

export interface MakerConfig {
  readonly secretKey: Uint8Array;
  readonly rpc: BeatExRpc;
  readonly market: MarketConfig;
  /** Quote atoms quoted at the levels nearest the reference price. */
  readonly levelQuoteAtoms: bigint;
  /** SOL lent to each maker wallet for rent and fees, swept back at the end. */
  readonly solLamports: bigint;
}

export function makerConfigFromEnv(market: MarketConfig): MakerConfig {
  const secret = process.env.FAUCET_SECRET_KEY;
  if (secret === undefined || secret.trim() === "") {
    throw new MakerError("The maker is not configured.", 503);
  }
  let secretKey: Uint8Array;
  try {
    secretKey = Uint8Array.from(JSON.parse(secret) as number[]);
  } catch {
    throw new MakerError("The maker key is malformed.", 503);
  }
  if (secretKey.length !== 64) {
    throw new MakerError("The maker key is malformed.", 503);
  }
  return {
    secretKey,
    rpc: createBeatExRpc(process.env.FAUCET_RPC_URL ?? publicEnv.rpcUrl),
    market,
    levelQuoteAtoms: parseDecimalUnits(
      process.env.MAKER_LEVEL_QUOTE ?? "500",
      market.quote.decimals,
    ),
    solLamports: parseDecimalUnits(process.env.MAKER_SOL_AMOUNT ?? "0.05", 9),
  };
}

/** Whether `owner` has a BeatEx balance: anyone else has no order to fill. */
export async function isTrader(
  config: MakerConfig,
  owner: string,
): Promise<boolean> {
  let wallet: Address;
  try {
    wallet = address(owner);
  } catch {
    throw new MakerError("That is not a Solana address.", 400);
  }
  const [balance] = await derivePrivateBalancePda(
    address(config.market.address),
    wallet,
    address(publicEnv.programId),
  );
  return (await config.rpc.getAccount(balance)) !== null;
}

async function readMarket(config: MakerConfig): Promise<MarketAccount> {
  const account = await config.rpc.getAccount(address(config.market.address));
  if (account === null) throw new MakerError("The market is missing.", 503);
  return decodeMarket(account.data);
}

/**
 * The auction a new maker should target: the first to open once it is ready. It
 * prepares for longer than an auction lasts, so it quotes as that auction opens.
 */
export function nextAuctionId(
  market: MarketAccount,
  slot: bigint,
): bigint | null {
  if (auctionIdAtSlot(market, slot) === null) return null;
  const ready = auctionIdAtSlot(market, slot + MIN_PREPARATION_SLOTS);
  return ready === null ? null : ready + 1n;
}

export async function targetAuction(config: MakerConfig): Promise<bigint> {
  const [market, slot] = await Promise.all([
    readMarket(config),
    config.rpc.getSlot(),
  ]);
  const target = nextAuctionId(market, slot);
  if (target === null) throw new MakerError("The market has not started.", 503);
  return target;
}

/**
 * The one maker wallet for an auction. Deriving it means concurrent calls agree on
 * the wallet, so the chain itself decides which of them makes this auction.
 */
export function makerSeed(
  secretKey: Uint8Array,
  market: string,
  auctionId: bigint,
): Uint8Array {
  return createHash("sha256")
    .update("beatex/demo-maker/v1")
    .update(secretKey)
    .update(market)
    .update(auctionId.toString())
    .digest();
}

/**
 * A two-sided order around the reference price: bids below it and asks above,
 * growing away from the centre up to twice the nearest level. It needs no
 * knowledge of any other order, so trader orders stay sealed.
 */
export function makerCurve(
  grid: readonly AuctionGridLevel[],
  levelQuoteAtoms: bigint,
  maxQuantity: bigint,
): OrderCurve {
  const centre = (grid.length - 1) / 2;
  const buy = grid.map(() => 0n);
  const sell = grid.map(() => 0n);
  grid.forEach((level, index) => {
    const steps = Math.abs(index - centre);
    if (steps === 0) return;
    const nearest = levelQuoteAtoms / level.price;
    const grown = nearest + (nearest * BigInt(steps - 1)) / 5n;
    const capped = grown > nearest * 2n ? nearest * 2n : grown;
    const quantity = capped > maxQuantity ? maxQuantity : capped;
    if (index < centre) buy[index] = quantity;
    else sell[index] = quantity;
  });
  return { buy, sell };
}

/**
 * Atoms to deposit before the auction's price is known, from an estimate of it.
 * The curve's largest entry is twice the nearest level, in either asset.
 */
export function makerDeposit(
  estimatedPrice: bigint,
  levelQuoteAtoms: bigint,
): { readonly baseAtoms: bigint; readonly quoteAtoms: bigint } {
  const largest = levelQuoteAtoms * 2n;
  return {
    baseAtoms:
      ((largest / estimatedPrice) * BASE_UNIT_SIZE * DEPOSIT_MARGIN_PERCENT) /
      100n,
    quoteAtoms: (largest * DEPOSIT_MARGIN_PERCENT) / 100n,
  };
}

/** The price `auctionId` would use if it opened now. */
async function estimatePrice(
  config: MakerConfig,
  market: MarketAccount,
  auctionId: bigint,
): Promise<bigint> {
  const oracle = hasOracle(market)
    ? await config.rpc.getAccount(address(market.oraclePriceAccount))
    : null;
  const preview = previewReferencePrice(
    market,
    auctionId,
    oracle === null ? null : decodePythPriceUpdate(oracle.data),
  );
  if (preview === null) throw new Error("No reference price is available.");
  return preview.referencePrice;
}

function clientFor(
  config: MakerConfig,
  signer: KeyPairSigner,
  prover: BeatExProver,
): BeatExClient {
  return createBeatExClient({
    rpc: config.rpc,
    signer: createKitSignerAdapter(signer),
    prover,
    storage: new InMemoryPrivateStateStorage(),
    cluster: publicEnv.cluster,
    programId: publicEnv.programId,
  });
}

async function waitForFinish(
  client: BeatExClient,
  market: string,
  auctionId: bigint,
  deadline: number,
): Promise<AuctionState> {
  for (;;) {
    const auction = await client.getAuction(market, auctionId);
    if (auction !== null && isAuctionResult(auction)) return auction.state;
    if (Date.now() >= deadline) {
      throw new Error(`Auction ${auctionId} did not finish in time.`);
    }
    await sleep(AUCTION_POLL_MS);
  }
}

/** Open the running auction. Two callers can race, and either outcome will do. */
async function openRunningAuction(client: BeatExClient, market: string) {
  try {
    await client.initializeCurrentAuction(market);
  } catch (error) {
    const auctionId = await client.getCurrentAuctionId(market);
    if ((await client.getAuction(market, auctionId)) === null) throw error;
  }
}

/** Open the running auction now, so a trader's order can go straight into it. */
export async function openCurrentAuction(config: MakerConfig): Promise<void> {
  const faucet = await createKeyPairSignerFromBytes(config.secretKey);
  // Opening an auction proves nothing, so this call never waits for the keys.
  const client = clientFor(config, faucet, {} as BeatExProver);
  await openRunningAuction(client, config.market.address);
}

/**
 * Open `auctionId` the moment the program allows, unless someone else did. An
 * auction can last under ten seconds, so every second here is one the quote loses.
 */
async function openWhenDue(
  config: MakerConfig,
  client: BeatExClient,
  market: MarketAccount,
  auctionId: bigint,
) {
  const address = config.market.address;
  const { openSlot, submissionEndSlot } = auctionWindow(market, auctionId);
  for (;;) {
    const slot = await config.rpc.getSlot("processed");
    const left = openSlot - OPEN_LEAD_SLOTS - slot;
    if (left <= 0n) break;
    await sleep(Number(left * FASTEST_SLOT_MS));
  }
  for (;;) {
    try {
      await client.initializeAuction(address, auctionId);
      break;
    } catch (error) {
      if ((await client.getAuction(address, auctionId)) !== null) break;
      if ((await config.rpc.getSlot("processed")) >= submissionEndSlot) {
        throw error;
      }
      await sleep(OPEN_POLL_MS);
    }
  }
  const auction = await client.getAuction(address, auctionId);
  if (auction === null || isAuctionResult(auction)) {
    throw new Error(`Auction ${auctionId} is not open.`);
  }
  return auction;
}

/**
 * Open `auctionId` and quote into it from a wallet that lives only for this call,
 * so no private state outlives it. The tokens are minted for the occasion and left
 * in the vault; a failure costs only test funds.
 */
export async function runMaker(
  config: MakerConfig,
  prover: BeatExProver,
  auctionId: bigint,
  log: (step: string, detail?: unknown) => void,
): Promise<void> {
  const started = Date.now();
  const deadline = started + SETTLE_DEADLINE_MS;
  const market = config.market.address;
  const faucet = await createKeyPairSignerFromBytes(config.secretKey);
  const maker = await createKeyPairSignerFromPrivateKeyBytes(
    makerSeed(config.secretKey, market, auctionId),
  );
  const [privateBalance] = await derivePrivateBalancePda(
    address(market),
    maker.address,
    address(publicEnv.programId),
  );
  const [makerAccount, balanceAccount] = await config.rpc.getMultipleAccounts([
    maker.address,
    privateBalance,
  ]);
  if (balanceAccount !== null || (makerAccount?.lamports ?? 0n) > 0n) {
    log("skip", { auctionId, reason: "auction already has a maker" });
    return;
  }

  const marketState = await readMarket(config);
  // Opening runs alongside preparation, so a slow deposit can't delay the trader.
  const opener = clientFor(config, faucet, prover);
  const opening = openWhenDue(config, opener, marketState, auctionId);
  opening.catch(() => undefined);

  const baseMint = address(marketState.baseMint);
  const quoteMint = address(marketState.quoteMint);
  const [baseAccount, quoteAccount, estimate] = await Promise.all([
    associatedTokenAddress(maker.address, baseMint),
    associatedTokenAddress(maker.address, quoteMint),
    estimatePrice(config, marketState, auctionId),
  ]);
  const { baseAtoms, quoteAtoms } = makerDeposit(
    estimate,
    config.levelQuoteAtoms,
  );

  // Creating the wallet is the claim on this auction: of two concurrent calls,
  // only one transaction can land.
  try {
    await sendInstructions(config.rpc, faucet, [
      createSystemAccount(faucet.address, maker, config.solLamports),
      createAssociatedTokenAccountIdempotent(
        faucet.address,
        maker.address,
        baseMint,
        baseAccount,
      ),
      createAssociatedTokenAccountIdempotent(
        faucet.address,
        maker.address,
        quoteMint,
        quoteAccount,
      ),
      mintToChecked(
        baseMint,
        baseAccount,
        faucet.address,
        baseAtoms,
        config.market.base.decimals,
      ),
      mintToChecked(
        quoteMint,
        quoteAccount,
        faucet.address,
        quoteAtoms,
        config.market.quote.decimals,
      ),
    ]);
  } catch (error) {
    if ((await config.rpc.getAccount(maker.address)) === null) throw error;
    log("skip", { auctionId, reason: "another call claimed this auction" });
    return;
  }
  log("funded", { auctionId, ms: Date.now() - started });

  try {
    const client = clientFor(config, maker, prover);
    await client.deposit({
      market,
      baseSource: baseAccount,
      quoteSource: quoteAccount,
      baseAmount: baseAtoms,
      quoteAmount: quoteAtoms,
    });
    log("deposited", { ms: Date.now() - started });

    const held = baseAtoms / BASE_UNIT_SIZE;
    const limit = BigInt(marketState.maxOrderQuantity);
    const quote = (auction: Awaited<typeof opening>) =>
      client.placeOrder({
        market,
        curve: makerCurve(
          buildAuctionGrid(auction),
          config.levelQuoteAtoms,
          held < limit ? held : limit,
        ),
        initializeAuction: false,
      });
    const auction = await opening;
    log("opened", { ms: Date.now() - started });
    // An auction can end before a slow quote lands; the next one still needs it.
    const order = await quote(auction).catch(async (error: unknown) => {
      log("missed", { auctionId, error: String(error).slice(0, 120) });
      return quote(
        await openWhenDue(config, opener, marketState, auctionId + 1n),
      );
    });
    log("quoted", {
      auctionId: order.auctionId,
      ms: Date.now() - started,
      signature: order.transaction.signature,
    });

    const state = await waitForFinish(
      client,
      market,
      order.auctionId,
      deadline,
    );
    log("finished", { state, ms: Date.now() - started });
    // Settling reclaims the rent on the order's accounts.
    if (state === AuctionState.Settled) {
      await client.settleToBalance({ market, auctionId: order.auctionId });
    } else {
      await client.refundToBalance({ market, auctionId: order.auctionId });
    }
  } finally {
    const swept = await sweep(config, maker, faucet.address, [
      baseAccount,
      quoteAccount,
    ]);
    log("swept", { lamports: swept, ms: Date.now() - started });
  }
}

/** Return what is left of the lent SOL, closing any token account now empty. */
async function sweep(
  config: MakerConfig,
  maker: KeyPairSigner,
  faucet: Address,
  tokenAccounts: readonly Address[],
): Promise<bigint> {
  const [wallet, ...accounts] = await config.rpc.getMultipleAccounts([
    maker.address,
    ...tokenAccounts,
  ]);
  const lamports = (wallet?.lamports ?? 0n) - TRANSACTION_FEE_LAMPORTS;
  if (lamports <= 0n) return 0n;
  await sendInstructions(config.rpc, maker, [
    ...tokenAccounts.flatMap((account, index) => {
      const data = accounts[index]?.data;
      return data !== undefined && tokenAccountAmount(data) === 0n
        ? [closeTokenAccount(account, faucet, maker.address)]
        : [];
    }),
    systemTransfer(maker.address, faucet, lamports),
  ]);
  return lamports;
}
