import type { AccountInfo, BeatExRpc } from "@beatex/sdk";
import {
  address,
  generateKeyPairSigner,
  getCompiledTransactionMessageDecoder,
  getTransactionDecoder,
} from "@solana/kit";
import { describe, expect, it } from "vitest";

import { MARKETS } from "@/lib/config/markets";

import { FaucetError, dripTestFunds, type FaucetConfig } from "./faucet";

const BASE_MINT = address("33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm");
const QUOTE_MINT = address("7Fg3pJw2stGKi7WTQRYNsWYqYrFcuwziD8C5HpH9s9Ng");
const RECIPIENT = "GYuK4aof3fS7M1dzYzwLrrdokdQsVieuPfVuEBxjCitq";

function tokenAccount(amount: bigint): AccountInfo {
  const data = new Uint8Array(165);
  new DataView(data.buffer).setBigUint64(64, amount, true);
  return {
    owner: "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    lamports: 1n,
    executable: false,
    data,
  };
}

async function setup(accounts: readonly (AccountInfo | null)[]) {
  const sent: Uint8Array[] = [];
  const rpc = {
    getMultipleAccounts: async () => accounts,
    getLatestBlockhash: async () => ({
      blockhash: "EkSnNWid2cvwEVnVx9aBqawnmiCNiDgp3gUdkDPTKN1N",
      lastValidBlockHeight: 100n,
    }),
    sendRawTransaction: async (bytes: Uint8Array) => {
      sent.push(bytes);
      return "signature";
    },
    confirmTransaction: async () => ({
      signature: "signature",
      slot: 1n,
      confirmationStatus: "confirmed",
    }),
  } as unknown as BeatExRpc;
  const faucet = await generateKeyPairSigner(true);
  const secretKey = new Uint8Array(64);
  secretKey.set(
    new Uint8Array(
      await crypto.subtle.exportKey("pkcs8", faucet.keyPair.privateKey),
    ).slice(-32),
    0,
  );
  secretKey.set(
    new Uint8Array(
      await crypto.subtle.exportKey("raw", faucet.keyPair.publicKey),
    ),
    32,
  );
  const config: FaucetConfig = {
    secretKey,
    rpc,
    baseMint: BASE_MINT,
    quoteMint: QUOTE_MINT,
    market: MARKETS[0]!,
    baseAtoms: 5_000_000n,
    quoteAtoms: 5_000_000_000n,
    solTargetLamports: 50_000_000n,
  };
  return { config, sent };
}

function wallet(lamports: bigint): AccountInfo {
  return {
    owner: "11111111111111111111111111111111",
    lamports,
    executable: false,
    data: new Uint8Array(),
  };
}

const RICH_FAUCET = wallet(1_000_000_000n);

function instructionCount(bytes: Uint8Array) {
  const transaction = getTransactionDecoder().decode(bytes);
  const message = getCompiledTransactionMessageDecoder().decode(
    transaction.messageBytes,
  );
  return "instructions" in message ? message.instructions.length : 0;
}

describe("dripTestFunds", () => {
  it("sends a new wallet SOL plus one drip of each token", async () => {
    const { config, sent } = await setup([
      null,
      RICH_FAUCET,
      null,
      null,
      tokenAccount(1_000_000_000n),
      tokenAccount(1_000_000_000_000n),
    ]);
    const result = await dripTestFunds(config, RECIPIENT);
    expect(result).toMatchObject({
      baseAtoms: 5_000_000n,
      quoteAtoms: 5_000_000_000n,
      lamports: 50_000_000n,
    });
    // One SOL transfer, then an account creation and TransferChecked per token.
    expect(sent).toHaveLength(1);
    expect(instructionCount(sent[0]!)).toBe(5);
  });

  it("tops up only SOL for a funded wallet that is running low", async () => {
    const { config, sent } = await setup([
      wallet(4_000_000n),
      RICH_FAUCET,
      tokenAccount(5_000_000n),
      tokenAccount(5_000_000_000n),
      tokenAccount(1n),
      tokenAccount(1n),
    ]);
    const result = await dripTestFunds(config, RECIPIENT);
    expect(result).toMatchObject({
      baseAtoms: 0n,
      quoteAtoms: 0n,
      lamports: 46_000_000n,
    });
    expect(instructionCount(sent[0]!)).toBe(1);
  });

  it("refuses wallets missing nothing, and an empty faucet", async () => {
    const funded = await setup([
      wallet(30_000_000n),
      RICH_FAUCET,
      tokenAccount(5_000_000n),
      tokenAccount(5_000_000_000n),
      tokenAccount(1n),
      tokenAccount(1n),
    ]);
    await expect(dripTestFunds(funded.config, RECIPIENT)).rejects.toMatchObject(
      { status: 429 },
    );

    const noTokens = await setup([
      null,
      RICH_FAUCET,
      null,
      null,
      tokenAccount(1n),
      tokenAccount(1n),
    ]);
    await expect(
      dripTestFunds(noTokens.config, RECIPIENT),
    ).rejects.toMatchObject({ status: 503 });

    const noSol = await setup([
      null,
      wallet(20_000_000n),
      null,
      null,
      tokenAccount(1_000_000_000n),
      tokenAccount(1_000_000_000_000n),
    ]);
    await expect(dripTestFunds(noSol.config, RECIPIENT)).rejects.toMatchObject({
      status: 503,
    });
    expect([...noTokens.sent, ...noSol.sent]).toHaveLength(0);
  });

  it("rejects malformed addresses", async () => {
    const { config } = await setup([]);
    await expect(
      dripTestFunds(config, "not-an-address"),
    ).rejects.toBeInstanceOf(FaucetError);
  });
});
