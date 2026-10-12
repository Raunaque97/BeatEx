import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";
import {
  getCompiledTransactionMessageDecoder,
  getTransactionDecoder,
} from "@solana/kit";
import {
  ACCOUNT_DISCRIMINATORS,
  BEATEX_PROGRAM_ADDRESS,
  INSTRUCTION_DISCRIMINATORS,
  DEVNET_FAST_ORDER_CIRCUIT_VERSION,
  GENERATED_DEPLOYMENT,
  InMemoryPrivateStateStorage,
  QueueCircuitKind,
  SYSTEM_PROGRAM_ADDRESS,
  TOKEN_2022_PROGRAM_ADDRESS,
  createBeatExClient,
  createBeatExRpc,
  createKitTransactionCompiler,
  deriveAuctionPda,
  deriveActiveOrderPda,
  deriveKeyEpochPda,
  deriveMarketPda,
  derivePrivateBalancePda,
  deriveQueuedOrderPda,
  deriveRolloverOrderPda,
  deriveSubmissionCounterPda,
  deriveVerificationKeyPda,
  deriveVaultAuthorityPda,
  encodeBindRolloverSettlement,
  encodeDepositCollateral,
  encodeInitializeAuction,
  encodeInitializePrivateBalance,
  encodeBindRolloverOrder,
  encodeCancelActiveOrder,
  encodePrepareRollover,
  encodeRolloverOrder,
  encodeSubmitQueuedOrder,
  encodeVerifyRolloverProof,
  encodeWithdrawPublicBalance,
  baseTokensToUnits,
  baseUnitsToTokens,
  buildAuctionGrid,
  priceAtGridOffset,
  quoteTokensToUnits,
  quoteUnitsToTokens,
  tokenPriceToUnits,
} from "../dist/index.js";

const TBTC = "33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm";
const TUSD = "7Fg3pJw2stGKi7WTQRYNsWYqYrFcuwziD8C5HpH9s9Ng";
const MARKET = "3haCd5iqRJuk9FGVFRdkW6jMnKQL4CH4vgwdpVc8XzxS";
const OWNER = "Db3M2ef9iDH1rVeFhmQzoDnywbu4wdQWDonqGfs2ATQS";

const fixed = (length, fill = 0) => new Uint8Array(length).fill(fill);
const proof = {
  proofA: fixed(64, 1),
  proofB: fixed(128, 2),
  proofC: fixed(64, 3),
  publicInput: fixed(32, 4),
};

test("generated protocol metadata keeps production order submission at v2 and Devnet-fast at v3", () => {
  assert.equal(GENERATED_DEPLOYMENT.protocol.circuitVersions.ORDER_SUBMISSION, 2);
  assert.equal(GENERATED_DEPLOYMENT.protocol.circuitVersions.DEVNET_FAST_ORDER, 3);
});

test("human token quantities convert exactly to configured units", () => {
  assert.equal(baseTokensToUnits("0.000001"), 1n);
  assert.equal(baseUnitsToTokens(1n), "0.000001");
  assert.equal(quoteTokensToUnits("1.25"), 1_250_000n);
  assert.equal(quoteUnitsToTokens(1_250_000n), "1.25");
  assert.equal(tokenPriceToUnits("65000"), 65_000n);
  assert.throws(() => baseTokensToUnits("0.00000001"), /exact multiple/);
});

test("basis-point grid uses checked half-up prices and rejects duplicates", () => {
  assert.equal(priceAtGridOffset(99_950n, 25, 1), 100_200n);
  const grid = buildAuctionGrid({ referencePrice: 100_000n, gridStepBps: 25, gridSize: 17 });
  assert.equal(grid[8].price, 100_000n);
  assert.equal(grid[0].offset, -8);
  assert.ok(grid.every((level, index) => index === 0 || grid[index - 1].price < level.price));
  assert.throws(
    () => buildAuctionGrid({ referencePrice: 100n, gridStepBps: 1, gridSize: 17 }),
    /duplicate or unordered/,
  );
});

test("Devnet PDA derivation matches the deployed market", async () => {
  const [market] = await deriveMarketPda({ baseMint: TBTC, quoteMint: TUSD, marketNonce: 2n });
  assert.equal(market, MARKET);
  const [vault] = await deriveVaultAuthorityPda(market);
  assert.equal(vault, "2gbWNfRTH37Nvc3k3FCayaT1FvXysDTF8KKE3SpcWnWN");
  const [epoch] = await deriveKeyEpochPda(market, 1n);
  assert.equal(epoch, "AoJ755HzDcFx8VZB3rpotAWRszxv7dCVZYX9WSRe3abe");
  const [balance] = await derivePrivateBalancePda(market, OWNER);
  assert.equal(typeof balance, "string");
  const [auction] = await deriveAuctionPda(market, 1007n);
  assert.equal(typeof auction, "string");
  const [queuedOrder] = await deriveQueuedOrderPda(auction, OWNER, 7n);
  const [differentNonceOrder] = await deriveQueuedOrderPda(auction, OWNER, 8n);
  const [differentOwnerOrder] = await deriveQueuedOrderPda(auction, MARKET, 7n);
  assert.notEqual(queuedOrder, differentNonceOrder);
  assert.notEqual(queuedOrder, differentOwnerOrder);
});

test("instruction encoders preserve frozen Anchor wire lengths", () => {
  assert.equal(encodeInitializeAuction(42n).length, 16);
  assert.equal(
    encodeDepositCollateral({
      baseAmount: 1n,
      quoteAmount: 2n,
      newEncryptedBase: fixed(64),
      newEncryptedQuote: fixed(64),
      proof,
    }).length,
    440,
  );
  assert.equal(
    encodeSubmitQueuedOrder({
      clientOrderNonce: 3n,
      orderCommitment: fixed(32),
      ciphertexts: fixed(2176),
      newEncryptedBase: fixed(64),
      newEncryptedQuote: fixed(64),
      proof,
    }).length,
    2644,
  );
  assert.equal(
    encodeVerifyRolloverProof({
      circuitKind: QueueCircuitKind.RolloverSettlement,
      expectedPublicInput: fixed(32),
      proof,
    }).length,
    329,
  );
  assert.equal(
    encodeBindRolloverSettlement({
      settlement: {
        terminalNullifier: fixed(32),
        creditedEncryptedBase: fixed(64),
        creditedEncryptedQuote: fixed(64),
      },
      expectedPublicInput: fixed(32),
    }).length,
    200,
  );
  assert.equal(encodeRolloverOrder().length, 8);
  assert.equal(
    encodeCancelActiveOrder({
      clientOrderNonce: 9n,
      releasedEncryptedBase: fixed(64),
      releasedEncryptedQuote: fixed(64),
      proof,
    }).length,
    432,
  );
  assert.equal(encodePrepareRollover().length, 8);
});

test("private-state journal records broadcast before atomic confirmation", async () => {
  const storage = new InMemoryPrivateStateStorage();
  const key = { cluster: "devnet", programId: BEATEX_PROGRAM_ADDRESS, market: MARKET, owner: OWNER };
  const state = {
    balanceRevision: 0n,
    keyEpoch: "AoJ755HzDcFx8VZB3rpotAWRszxv7dCVZYX9WSRe3abe",
    balance: {
      baseValue: 0n,
      quoteValue: 0n,
      baseRandomness: fixed(32),
      quoteRandomness: fixed(32),
    },
    encryptedBase: fixed(64),
    encryptedQuote: fixed(64),
    activeOrder: null,
  };
  await storage.stage(key, {
    id: "transition-1",
    operation: "initialize",
    expectedStorageRevision: null,
    nextState: state,
  });
  await storage.markBroadcast(key, "transition-1", "signature", "blockhash", 99n);
  const pending = await storage.load(key);
  assert.equal(pending.pending?.transactionSignature, "signature");
  const record = await storage.commitConfirmed(key, "transition-1", {
    signature: "signature",
    slot: 10n,
    confirmationStatus: "confirmed",
  });
  assert.equal(record.storageRevision, 0n);
  assert.equal(record.state.balanceRevision, 0n);
  assert.equal((await storage.load(key)).pending, null);
});

test("failed pre-sign simulation never requests a wallet signature or stages state", async () => {
  const storage = new InMemoryPrivateStateStorage();
  const calls = [];
  const client = createBeatExClient({
    cluster: "devnet",
    storage,
    prover: {},
    rpc: {
      getAccount: async () => null,
      getLatestBlockhash: async () => ({
        blockhash: "11111111111111111111111111111111",
        lastValidBlockHeight: 99n,
      }),
      simulateTransaction: async () => {
        calls.push("simulate");
        return {
          err: { InstructionError: [0, { Custom: 6001 }] },
          logs: ["Program log: Error: insufficient funds"],
        };
      },
      sendRawTransaction: async () => { calls.push("send"); },
    },
    signer: {
      address: OWNER,
      signTransaction: async (bytes) => { calls.push("sign"); return bytes; },
    },
  });
  client.getMarket = async () => ({ currentKeyEpoch: "AoJ755HzDcFx8VZB3rpotAWRszxv7dCVZYX9WSRe3abe" });
  await assert.rejects(client.deposit({
    market: MARKET,
    baseSource: OWNER,
    quoteSource: OWNER,
    baseAmount: 1n,
    quoteAmount: 0n,
  }), (error) => error.code === "SIMULATION_FAILED" && error.message.includes("insufficient funds"));
  assert.deepEqual(calls, ["simulate"]);
  const snapshot = await storage.load({ cluster: "devnet", programId: BEATEX_PROGRAM_ADDRESS, market: MARKET, owner: OWNER });
  assert.equal(snapshot.record, null);
  assert.equal(snapshot.pending, null);
});

test("RPC simulation uses the same commitment as SDK blockhash acquisition", async () => {
  const rpc = createBeatExRpc("https://api.devnet.solana.com", {
    fetch: async (_url, options) => {
      const request = JSON.parse(options.body);
      assert.equal(request.method, "simulateTransaction");
      assert.equal(request.params[1].sigVerify, false);
      assert.equal(request.params[1].commitment, "confirmed");
      return new Response(JSON.stringify({ result: { value: { err: null, logs: [] } } }));
    },
  });
  assert.equal((await rpc.simulateTransaction(Uint8Array.of(1, 2, 3))).err, null);
});

test("Kit compiler emits an extended v1 transaction below the configured cap", async () => {
  const compiler = createKitTransactionCompiler();
  const bytes = await compiler.compileTransaction({
    feePayer: OWNER,
    recentBlockhash: {
      blockhash: "11111111111111111111111111111111",
      lastValidBlockHeight: 1n,
    },
    instructions: [
      {
        programId: BEATEX_PROGRAM_ADDRESS,
        accounts: [{ address: MARKET, writable: false, signer: false }],
        data: encodeRolloverOrder(),
      },
    ],
    config: {
      version: 1,
      maxTransactionBytes: 4096,
      computeUnitLimit: 1_400_000,
      loadedAccountsDataSizeLimit: 2 * 1024 * 1024,
      heapSize: 256 * 1024,
      priorityFeeLamports: 0n,
    },
  });
  assert.ok(bytes.length > 0 && bytes.length < 4096);
});

test("private-balance initialization, deposit, and withdrawal fit legacy transactions", async () => {
  const compiler = createKitTransactionCompiler();
  const keyEpoch = "GUmGUhaqt7oAt1YkyrAT45CbApGHPxbJTdL73yL6mrED";
  const verificationKey = "6RYzpuCsjK9S8gjdvFvNu5nn1FUMzSe8K1f2Cbq2GqqH";
  const privateBalance = "HszvZYc6jdgTUSjknGM7GWA6a3YVb9NdZcLx1kyNQ4zq";
  const baseSource = "AELxK7o5px97XjhMYuEBQunMBXTCQUebqbyzeqEvcnoJ";
  const quoteSource = "8fRxn1rXBtJoBSVNT5CiVXQZzBdib5CAbBJ1da2gH6Up";
  const baseVault = "66hcLz9vzMcnb98yjiUa1aiJWZUnyDCg9jjbxAXerxTU";
  const quoteVault = "BukhQVo4ebbCbdPaithQvAT8yqhYLHh2wmvxfVbr21j2";
  const vaultAuthority = "GX352jffG1zdZ3juFr2HVyoCc5vJ5LWQJWmtDcBM9np7";
  const account = (address, writable = false, signer = false) => ({
    address,
    writable,
    signer,
  });
  const common = [
    account(OWNER, true, true),
    account(MARKET),
    account(keyEpoch),
    account(verificationKey),
    account(privateBalance, true),
    account(TBTC),
    account(TUSD),
  ];
  const cases = [
    {
      name: "private-balance initialization",
      accounts: [
        account(OWNER, true, true),
        account(MARKET),
        account(keyEpoch),
        account(privateBalance, true),
        account(SYSTEM_PROGRAM_ADDRESS),
      ],
      data: encodeInitializePrivateBalance(fixed(64), fixed(64)),
      expectedSize: 495,
    },
    {
      name: "deposit",
      accounts: [
        ...common,
        account(baseSource, true),
        account(quoteSource, true),
        account(baseVault, true),
        account(quoteVault, true),
        account(vaultAuthority),
        account(TOKEN_2022_PROGRAM_ADDRESS),
      ],
      data: encodeDepositCollateral({
        baseAmount: 1n,
        quoteAmount: 1n,
        newEncryptedBase: fixed(64),
        newEncryptedQuote: fixed(64),
        proof,
      }),
      expectedSize: 1063,
    },
    {
      name: "withdrawal",
      accounts: [
        ...common,
        account(baseVault, true),
        account(quoteVault, true),
        account(baseSource, true),
        account(quoteSource, true),
        account(vaultAuthority),
        account(TOKEN_2022_PROGRAM_ADDRESS),
      ],
      data: encodeWithdrawPublicBalance({
        baseAmount: 1n,
        quoteAmount: 1n,
        remainingEncryptedBase: fixed(64),
        remainingEncryptedQuote: fixed(64),
        proof,
      }),
      expectedSize: 1063,
    },
  ];

  for (const item of cases) {
    const bytes = await compiler.compileTransaction({
      feePayer: OWNER,
      recentBlockhash: {
        blockhash: "11111111111111111111111111111111",
        lastValidBlockHeight: 1n,
      },
      instructions: [
        {
          programId: BEATEX_PROGRAM_ADDRESS,
          accounts: item.accounts,
          data: item.data,
        },
      ],
      config: {
        version: "legacy",
        maxTransactionBytes: 1232,
        computeUnitLimit: 1_400_000,
        loadedAccountsDataSizeLimit: 2 * 1024 * 1024,
        heapSize: 256 * 1024,
        priorityFeeLamports: 0n,
      },
    });
    const decoded = getTransactionDecoder().decode(bytes);
    const message = getCompiledTransactionMessageDecoder().decode(
      decoded.messageBytes,
    );
    assert.equal(message.version, "legacy", `${item.name} must use the legacy wire format`);
    assert.equal(bytes.length, item.expectedSize, `${item.name} legacy wire size changed`);
    assert.ok(bytes.length <= 1232);
  }
});

test("the frozen six-instruction rollover remains exactly 4,093 bytes", async () => {
  const owner = "GYuK4aof3fS7M1dzYzwLrrdokdQsVieuPfVuEBxjCitq";
  const keyEpoch = "AoJ755HzDcFx8VZB3rpotAWRszxv7dCVZYX9WSRe3abe";
  const [oldAuction] = await deriveAuctionPda(MARKET, 822n);
  const [nextAuction] = await deriveAuctionPda(MARKET, 823n);
  const [nextCounter] = await deriveSubmissionCounterPda(nextAuction);
  const [oldOrder] = await deriveQueuedOrderPda(oldAuction, owner, 1n);
  const [oldActive] = await deriveActiveOrderPda(oldAuction, owner);
  const [nextOrder] = await deriveRolloverOrderPda(nextAuction, owner);
  const [nextActive] = await deriveActiveOrderPda(nextAuction, owner);
  const [balance] = await derivePrivateBalancePda(MARKET, owner);
  const [rolloverKey] = await deriveVerificationKeyPda(
    MARKET,
    QueueCircuitKind.RolloverSettlement,
    1,
  );
  const [orderKey] = await deriveVerificationKeyPda(
    MARKET,
    QueueCircuitKind.OrderSubmission,
    DEVNET_FAST_ORDER_CIRCUIT_VERSION,
  );
  const settlement = {
    terminalNullifier: fixed(32),
    creditedEncryptedBase: fixed(64),
    creditedEncryptedQuote: fixed(64),
  };
  const nextOrderArgs = {
    clientOrderNonce: 1n,
    orderCommitment: fixed(32),
    ciphertexts: fixed(2176),
    newEncryptedBase: fixed(64),
    newEncryptedQuote: fixed(64),
  };
  const account = (address, writable = false, signer = false) => ({ address, writable, signer });
  const instruction = (accounts, data) => ({
    programId: BEATEX_PROGRAM_ADDRESS,
    accounts,
    data,
  });
  const sysvar = "Sysvar1nstructions1111111111111111111111111";
  const system = "11111111111111111111111111111111";
  const instructions = [
    instruction(
      [
        account(owner, true, true), account(MARKET), account(nextAuction), account(nextCounter),
        account(nextOrder, true), account(nextActive, true), account(sysvar), account(system),
      ],
      encodePrepareRollover(),
    ),
    instruction(
      [account(MARKET), account(rolloverKey)],
      encodeVerifyRolloverProof({
        circuitKind: QueueCircuitKind.RolloverSettlement,
        expectedPublicInput: fixed(32),
        proof,
      }),
    ),
    instruction(
      [
        account(owner, false, true), account(MARKET), account(oldAuction), account(keyEpoch),
        account(oldOrder), account(oldActive), account(balance), account(rolloverKey), account(sysvar),
      ],
      encodeBindRolloverSettlement({ settlement, expectedPublicInput: fixed(32) }),
    ),
    instruction(
      [account(MARKET), account(orderKey)],
      encodeVerifyRolloverProof({
        circuitKind: QueueCircuitKind.OrderSubmission,
        expectedPublicInput: fixed(32),
        proof,
      }),
    ),
    instruction(
      [
        account(owner, false, true), account(MARKET), account(nextAuction), account(keyEpoch),
        account(orderKey), account(sysvar),
      ],
      encodeBindRolloverOrder({
        nextOrder: nextOrderArgs,
        creditedEncryptedBase: fixed(64),
        creditedEncryptedQuote: fixed(64),
        expectedPublicInput: fixed(32),
      }),
    ),
    instruction(
      [
        account(owner, true, true), account(MARKET), account(oldAuction), account(nextAuction),
        account(keyEpoch), account(oldOrder, true), account(oldActive, true),
        account(nextCounter, true), account(balance, true), account(rolloverKey), account(orderKey),
        account(nextOrder, true), account(nextActive, true), account(sysvar),
      ],
      encodeRolloverOrder(),
    ),
  ];
  const bytes = await createKitTransactionCompiler().compileTransaction({
    feePayer: owner,
    recentBlockhash: {
      blockhash: "11111111111111111111111111111111",
      lastValidBlockHeight: 1n,
    },
    instructions,
    config: {
      version: 1,
      maxTransactionBytes: 4096,
      computeUnitLimit: 1_400_000,
      loadedAccountsDataSizeLimit: 2 * 1024 * 1024,
      heapSize: 256 * 1024,
      priorityFeeLamports: 0n,
    },
  });
  assert.equal(bytes.length, 4093);
});

const IDL_PATH = new URL("../../../target/idl/beatex.json", import.meta.url);

test("hand-written instruction discriminators match the program IDL", { skip: !existsSync(IDL_PATH) }, () => {
  const idl = JSON.parse(readFileSync(IDL_PATH, "utf8"));
  const byName = new Map(idl.instructions.map((instruction) => [instruction.name, instruction.discriminator]));
  for (const [name, discriminator] of Object.entries(INSTRUCTION_DISCRIMINATORS)) {
    const snake = name.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
    assert.deepEqual([...discriminator], byName.get(snake), name);
  }
});

test("hand-written account discriminators match the program IDL", { skip: !existsSync(IDL_PATH) }, () => {
  const idl = JSON.parse(readFileSync(IDL_PATH, "utf8"));
  const byName = new Map(idl.accounts.map((account) => [account.name, account.discriminator]));
  for (const [name, discriminator] of Object.entries(ACCOUNT_DISCRIMINATORS)) {
    const pascal = name[0].toUpperCase() + name.slice(1);
    assert.deepEqual([...discriminator], byName.get(pascal), name);
  }
});
