import {
  createBeatExRpc,
  parseDecimalUnits,
  type BeatExRpc,
} from "@beatex/sdk";
import {
  address,
  appendTransactionMessageInstructions,
  createKeyPairSignerFromBytes,
  createTransactionMessage,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
  type Address,
  type Blockhash,
  type Instruction,
} from "@solana/kit";

import { SOL_TOP_UP_FLOOR_LAMPORTS } from "@/lib/config/faucet";
import type { MarketConfig } from "@/lib/config/markets";
import { publicEnv } from "@/lib/config/public-env";
import {
  associatedTokenAddress,
  createAssociatedTokenAccountIdempotent,
  systemTransfer,
  tokenAccountAmount,
  transferChecked,
} from "@/lib/solana/token";

/** SOL the faucet keeps back for its own fees and recipients' token-account rent. */
const FAUCET_SOL_RESERVE_LAMPORTS = 10_000_000n;

export class FaucetError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

export interface FaucetConfig {
  readonly secretKey: Uint8Array;
  readonly rpc: BeatExRpc;
  readonly baseMint: Address;
  readonly quoteMint: Address;
  readonly market: MarketConfig;
  /** Token atoms per drip. */
  readonly baseAtoms: bigint;
  readonly quoteAtoms: bigint;
  /** Wallets below the top-up floor are topped up to this many lamports. */
  readonly solTargetLamports: bigint;
}

export function faucetConfigFromEnv(
  market: MarketConfig,
  baseMint: string,
  quoteMint: string,
): FaucetConfig {
  const secret = process.env.FAUCET_SECRET_KEY;
  if (secret === undefined || secret.trim() === "") {
    throw new FaucetError(
      "The faucet is not configured on this deployment.",
      503,
    );
  }
  let secretKey: Uint8Array;
  try {
    secretKey = Uint8Array.from(JSON.parse(secret) as number[]);
  } catch {
    throw new FaucetError("The faucet key is malformed.", 503);
  }
  if (secretKey.length !== 64)
    throw new FaucetError("The faucet key is malformed.", 503);
  return {
    secretKey,
    rpc: createBeatExRpc(process.env.FAUCET_RPC_URL ?? publicEnv.rpcUrl),
    baseMint: address(baseMint),
    quoteMint: address(quoteMint),
    market,
    baseAtoms: parseDecimalUnits(
      process.env.FAUCET_BASE_AMOUNT ?? "0.05",
      market.base.decimals,
    ),
    quoteAtoms: parseDecimalUnits(
      process.env.FAUCET_QUOTE_AMOUNT ?? "5000",
      market.quote.decimals,
    ),
    solTargetLamports: parseDecimalUnits(
      process.env.FAUCET_SOL_AMOUNT ?? "0.05",
      9,
    ),
  };
}

export interface FaucetResult {
  readonly signature: string;
  readonly baseAtoms: bigint;
  readonly quoteAtoms: bigint;
  readonly lamports: bigint;
}

/**
 * Send whatever a demo wallet is missing: one drip of each test token it holds
 * less than, and SOL up to the target when it is below the top-up floor. A wallet
 * missing nothing is refused, which keeps the route stateless on serverless hosting.
 */
export async function dripTestFunds(
  config: FaucetConfig,
  recipientInput: string,
): Promise<FaucetResult> {
  let recipient: Address;
  try {
    recipient = address(recipientInput);
  } catch {
    throw new FaucetError("That is not a Solana address.", 400);
  }

  const faucet = await createKeyPairSignerFromBytes(config.secretKey);
  const [recipientBase, recipientQuote, faucetBase, faucetQuote] =
    await Promise.all([
      associatedTokenAddress(recipient, config.baseMint),
      associatedTokenAddress(recipient, config.quoteMint),
      associatedTokenAddress(faucet.address, config.baseMint),
      associatedTokenAddress(faucet.address, config.quoteMint),
    ]);
  const [
    recipientAccount,
    faucetAccount,
    baseAccount,
    quoteAccount,
    faucetBaseAccount,
    faucetQuoteAccount,
  ] = await config.rpc.getMultipleAccounts([
    recipient,
    faucet.address,
    recipientBase,
    recipientQuote,
    faucetBase,
    faucetQuote,
  ]);

  const held = (account: { readonly data: Uint8Array } | null) =>
    account === null ? 0n : tokenAccountAmount(account.data);
  const recipientLamports = recipientAccount?.lamports ?? 0n;
  const baseAtoms =
    held(baseAccount) < config.baseAtoms ? config.baseAtoms : 0n;
  const quoteAtoms =
    held(quoteAccount) < config.quoteAtoms ? config.quoteAtoms : 0n;
  const lamports =
    recipientLamports < SOL_TOP_UP_FLOOR_LAMPORTS &&
    recipientLamports < config.solTargetLamports
      ? config.solTargetLamports - recipientLamports
      : 0n;
  if (baseAtoms === 0n && quoteAtoms === 0n && lamports === 0n) {
    throw new FaucetError(
      "This wallet already has test funds. Deposit them to start trading.",
      429,
    );
  }
  if (
    held(faucetBaseAccount) < baseAtoms ||
    held(faucetQuoteAccount) < quoteAtoms ||
    (faucetAccount?.lamports ?? 0n) < lamports + FAUCET_SOL_RESERVE_LAMPORTS
  ) {
    throw new FaucetError("The faucet is empty. Please try again later.", 503);
  }

  const instructions: Instruction[] = [];
  if (lamports > 0n) {
    instructions.push(systemTransfer(faucet.address, recipient, lamports));
  }
  if (baseAtoms > 0n) {
    instructions.push(
      createAssociatedTokenAccountIdempotent(
        faucet.address,
        recipient,
        config.baseMint,
        recipientBase,
      ),
      transferChecked(
        faucetBase,
        config.baseMint,
        recipientBase,
        faucet.address,
        baseAtoms,
        config.market.base.decimals,
      ),
    );
  }
  if (quoteAtoms > 0n) {
    instructions.push(
      createAssociatedTokenAccountIdempotent(
        faucet.address,
        recipient,
        config.quoteMint,
        recipientQuote,
      ),
      transferChecked(
        faucetQuote,
        config.quoteMint,
        recipientQuote,
        faucet.address,
        quoteAtoms,
        config.market.quote.decimals,
      ),
    );
  }

  const { blockhash, lastValidBlockHeight } =
    await config.rpc.getLatestBlockhash("confirmed");
  const message = pipe(
    createTransactionMessage({ version: 0 }),
    (draft) => setTransactionMessageFeePayerSigner(faucet, draft),
    (draft) =>
      setTransactionMessageLifetimeUsingBlockhash(
        { blockhash: blockhash as Blockhash, lastValidBlockHeight },
        draft,
      ),
    (draft) => appendTransactionMessageInstructions(instructions, draft),
  );
  const signed = await signTransactionMessageWithSigners(message);
  const bytes = new Uint8Array(getTransactionEncoder().encode(signed));
  const signature = await config.rpc.sendRawTransaction(bytes);
  await config.rpc.confirmTransaction(signature, {
    commitment: "confirmed",
    blockhash,
    lastValidBlockHeight,
  });
  return { signature, baseAtoms, quoteAtoms, lamports };
}
