import { address } from "@solana/kit";
import { describe, expect, it } from "vitest";

import {
  associatedTokenAddress,
  tokenAccountAmount,
  transferChecked,
} from "./token";

describe("token helpers", () => {
  it("derives a known Token-2022 associated account", async () => {
    // From docs/DEVNET_ENVIRONMENT.md: the Pyth market maker's tBTC account.
    const owner = address("GYuK4aof3fS7M1dzYzwLrrdokdQsVieuPfVuEBxjCitq");
    const mint = address("33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm");
    expect(await associatedTokenAddress(owner, mint)).toBe(
      "CHSkbKWPe9cMNH8fgxWyzgn1VbZFnnBE1UheeveYzEUr",
    );
  });

  it("reads the amount from a token account's base layout", () => {
    const data = new Uint8Array(165);
    new DataView(data.buffer).setBigUint64(64, 123_456n, true);
    expect(tokenAccountAmount(data)).toBe(123_456n);
    expect(() => tokenAccountAmount(new Uint8Array(10))).toThrow();
  });

  it("encodes TransferChecked", () => {
    const a = address("11111111111111111111111111111111");
    const ix = transferChecked(a, a, a, a, 5_000_000n, 8);
    expect([...ix.data!]).toEqual([12, 64, 75, 76, 0, 0, 0, 0, 0, 8]);
  });
});
