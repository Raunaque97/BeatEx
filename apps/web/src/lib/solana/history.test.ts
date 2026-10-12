import { describe, expect, it } from "vitest";

import { describeTransaction } from "./history";

const OWNER = "AYQ4ZAPBEc5jU6ymuTFasFdJp2GGFx9Q8VDNHBqNmYdz";
const FAUCET = "6Ruc1n35VNQK5bQtneroGUiYMh4HMaNKPeM2E6cPTAVa";
const TBTC = "33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm";
const TUSD = "7Fg3pJw2stGKi7WTQRYNsWYqYrFcuwziD8C5HpH9s9Ng";

function token(index: number, mint: string, amount: string, decimals: number) {
  return {
    accountIndex: index,
    mint,
    owner: OWNER,
    uiTokenAmount: { amount, decimals },
  };
}

function tx(overrides: {
  keys: string[];
  logs: string[];
  pre?: number[];
  post?: number[];
  preTokens?: ReturnType<typeof token>[];
  postTokens?: ReturnType<typeof token>[];
  err?: unknown;
}) {
  return {
    slot: 510_259_080,
    blockTime: 1_791_758_982,
    transaction: { message: { accountKeys: overrides.keys } },
    meta: {
      err: overrides.err ?? null,
      fee: 5_000,
      logMessages: overrides.logs,
      preBalances: overrides.pre ?? overrides.keys.map(() => 0),
      postBalances: overrides.post ?? overrides.keys.map(() => 0),
      preTokenBalances: overrides.preTokens ?? [],
      postTokenBalances: overrides.postTokens ?? [],
    },
  };
}

describe("describeTransaction", () => {
  it("reads a deposit with the tokens that left the wallet", () => {
    const entry = describeTransaction(
      OWNER,
      "sig",
      tx({
        keys: [OWNER],
        logs: [
          "Program log: Instruction: DepositCollateral",
          "Program log: Instruction: TransferChecked",
        ],
        pre: [50_000_000],
        post: [49_995_000],
        preTokens: [
          token(1, TBTC, "5000000", 8),
          token(2, TUSD, "5000000000", 6),
        ],
        postTokens: [token(1, TBTC, "0", 8), token(2, TUSD, "3000000000", 6)],
      }),
    );
    expect(entry.kind).toBe("deposit");
    expect(entry.tokens).toEqual([
      { mint: TBTC, amount: "-0.05" },
      { mint: TUSD, amount: "-2000" },
    ]);
    // The owner paid only the fee, which isn't shown as a transfer.
    expect(entry.lamports).toBe(0n);
  });

  it("reads faucet funds as received, including SOL", () => {
    const entry = describeTransaction(
      OWNER,
      "sig",
      tx({
        keys: [FAUCET, OWNER],
        logs: ["Program log: Instruction: TransferChecked"],
        pre: [5_000_000_000, 0],
        post: [4_949_000_000, 50_000_000],
        postTokens: [token(2, TUSD, "5000000000", 6)],
      }),
    );
    expect(entry.kind).toBe("received");
    expect(entry.lamports).toBe(50_000_000n);
    expect(entry.tokens).toEqual([{ mint: TUSD, amount: "5000" }]);
  });

  it("names an order by its action even when it opened the batch, and marks failures", () => {
    expect(
      describeTransaction(
        OWNER,
        "sig",
        tx({
          keys: [OWNER],
          logs: [
            "Program log: Instruction: InitializeAuction",
            "Program log: Instruction: SubmitQueuedOrder",
          ],
        }),
      ).kind,
    ).toBe("order");
    const failed = describeTransaction(
      OWNER,
      "sig",
      tx({
        keys: [OWNER],
        logs: ["Program log: Instruction: InitializeAuction"],
        err: { InstructionError: [0, { Custom: 0 }] },
      }),
    );
    expect(failed).toMatchObject({ kind: "auction", failed: true });
  });
});
