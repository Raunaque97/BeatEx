import { describe, expect, it } from "vitest";

import {
  EncryptedStateStore,
  StateDecryptionError,
  deriveStateKey,
  stateKeyMessage,
  type RawStore,
} from "./state-encryption";

const scope = { cluster: "devnet", programId: "Program111", owner: "Owner111" };

function memoryStore(): RawStore & { readonly entries: Map<string, unknown> } {
  const entries = new Map<string, unknown>();
  return {
    entries,
    get: async (key) => entries.get(key),
    put: async (key, value) => {
      entries.set(key, value);
    },
  };
}

describe("private-state encryption", () => {
  it("names the wallet, cluster, and program in the signed message", () => {
    const text = new TextDecoder().decode(stateKeyMessage(scope));
    expect(text).toContain("Wallet: Owner111");
    expect(text).toContain("does not authorize a transaction");
  });

  it("round-trips values and stores only ciphertext", async () => {
    const key = await deriveStateKey(new Uint8Array(64).fill(9), scope);
    const raw = memoryStore();
    const store = new EncryptedStateStore(key, raw);

    expect(await store.getItem("slot")).toBeNull();
    await store.setItem("slot", '{"balance":"secret"}');
    expect(await store.getItem("slot")).toBe('{"balance":"secret"}');

    const sealed = raw.entries.get("slot") as { data: Uint8Array };
    expect(new TextDecoder().decode(sealed.data)).not.toContain("secret");
  });

  it("re-derives the same key from the same signature", async () => {
    const signature = new Uint8Array(64).fill(3);
    const raw = memoryStore();
    await new EncryptedStateStore(
      await deriveStateKey(signature, scope),
      raw,
    ).setItem("a", "x");
    const reopened = new EncryptedStateStore(
      await deriveStateKey(signature, scope),
      raw,
    );
    expect(await reopened.getItem("a")).toBe("x");
  });

  it("rejects a different wallet's key and ciphertext moved to another slot", async () => {
    const raw = memoryStore();
    await new EncryptedStateStore(
      await deriveStateKey(new Uint8Array(64).fill(1), scope),
      raw,
    ).setItem("a", "x");

    const other = new EncryptedStateStore(
      await deriveStateKey(new Uint8Array(64).fill(2), scope),
      raw,
    );
    await expect(other.getItem("a")).rejects.toBeInstanceOf(
      StateDecryptionError,
    );

    raw.entries.set("b", raw.entries.get("a"));
    const same = new EncryptedStateStore(
      await deriveStateKey(new Uint8Array(64).fill(1), scope),
      raw,
    );
    await expect(same.getItem("b")).rejects.toBeInstanceOf(
      StateDecryptionError,
    );
  });

  it("refuses signatures of the wrong length", async () => {
    await expect(deriveStateKey(new Uint8Array(32), scope)).rejects.toThrow();
  });
});
