import { afterEach, describe, expect, it, vi } from "vitest";

import { TxTracker, describeError } from "./tx-tracker";

afterEach(() => vi.useRealTimers());

describe("wallet signing", () => {
  it("releases the action after a stuck wallet and discards a late signature", async () => {
    vi.useFakeTimers();
    const tracker = new TxTracker();
    const listener = vi.fn();
    let resolveSignature!: (bytes: Uint8Array) => void;
    const signer = tracker.signer({
      address: "owner",
      signTransaction: () =>
        new Promise((resolve) => {
          resolveSignature = resolve;
        }),
    });
    const broadcast = vi.fn();
    const action = tracker.run(listener, async () => {
      const signed = await signer.signTransaction(Uint8Array.of(1));
      broadcast(signed);
    });
    const rejection = expect(action).rejects.toThrow(
      "This transaction was not broadcast",
    );
    await vi.advanceTimersByTimeAsync(60_000);
    await rejection;
    expect(listener).toHaveBeenLastCalledWith(
      expect.objectContaining({ phase: "failed" }),
    );
    resolveSignature(Uint8Array.of(2));
    await Promise.resolve();
    expect(broadcast).not.toHaveBeenCalled();
    await expect(tracker.run(listener, async () => "retry")).resolves.toBe(
      "retry",
    );
  });
});

describe("describeError", () => {
  it("explains an order that missed its auction's deadline", () => {
    expect(
      describeError(
        new Error(
          "BeatEx transaction simulation failed: Program log: AnchorError thrown in programs/beatex/src/lib.rs:656. Error Code: QueueInvariant. Error Number: 6046.",
        ),
      ),
    ).toBe(
      "This auction closed before your order arrived. Nothing was sent; place it again in the next auction.",
    );
  });
});
