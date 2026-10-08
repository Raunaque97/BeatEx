import assert from "node:assert/strict";
import test from "node:test";
import {
  createBeatExRpc,
  createBeatExClient,
  decodeAuction,
  decodeEncryptedPrivateBalance,
  decodeMarket,
  deriveAuctionPda,
  derivePrivateBalancePda,
  InMemoryPrivateStateStorage,
} from "../dist/index.js";

const MARKET = "3haCd5iqRJuk9FGVFRdkW6jMnKQL4CH4vgwdpVc8XzxS";
const OWNER = "Db3M2ef9iDH1rVeFhmQzoDnywbu4wdQWDonqGfs2ATQS";

test("decodes the deployed market, settled auction, and private balance", async () => {
  const rpc = createBeatExRpc("https://api.devnet.solana.com");
  const marketAccount = await rpc.getAccount(MARKET);
  assert.ok(marketAccount);
  const market = decodeMarket(marketAccount.data);
  assert.equal(market.baseMint, "33D68TXfhVb8qiKXBF6yob5opkaeD4ayPsNPketQdgxm");
  assert.equal(market.quoteMint, "7Fg3pJw2stGKi7WTQRYNsWYqYrFcuwziD8C5HpH9s9Ng");

  const [auctionAddress] = await deriveAuctionPda(MARKET, 1007n);
  const auctionAccount = await rpc.getAccount(auctionAddress);
  assert.ok(auctionAccount);
  const auction = decodeAuction(auctionAccount.data);
  assert.equal(auction.auctionId, 1007n);
  assert.equal(auction.clearingPrice, 830n);

  const [balanceAddress] = await derivePrivateBalancePda(MARKET, OWNER);
  const balanceAccount = await rpc.getAccount(balanceAddress);
  assert.ok(balanceAccount);
  const balance = decodeEncryptedPrivateBalance(balanceAccount.data);
  assert.equal(balance.owner, OWNER);
  assert.ok(balance.revision >= 4n);
});

test("high-level client reads the deployed auction and queue", async () => {
  const rpc = createBeatExRpc("https://api.devnet.solana.com");
  const client = createBeatExClient({
    rpc,
    signer: {
      address: OWNER,
      async signTransaction() {
        throw new Error("read-only integration signer");
      },
    },
    prover: {},
    storage: new InMemoryPrivateStateStorage(),
    cluster: "devnet",
  });
  const market = await client.getMarket(MARKET);
  assert.equal(market.currentKeyEpoch, "AoJ755HzDcFx8VZB3rpotAWRszxv7dCVZYX9WSRe3abe");
  const auction = await client.getAuction(MARKET, 1007n);
  assert.ok(auction);
  assert.equal(await client.getAuctionPhase(MARKET, 1007n), "settled");
  const progress = await client.getQueueProgress(MARKET, 1007n);
  assert.equal(progress.complete, true);
  assert.equal(progress.acceptedOrderCount, 1n);
});

test("high-level deposit assembles initialization and proved custody transitions", async () => {
  const live = createBeatExRpc("https://api.devnet.solana.com");
  const sent = [];
  let signatureIndex = 0;
  const rpc = {
    getAccount: (...args) => live.getAccount(...args),
    getMultipleAccounts: (...args) => live.getMultipleAccounts(...args),
    getSlot: (...args) => live.getSlot(...args),
    async getLatestBlockhash() {
      return { blockhash: "11111111111111111111111111111111", lastValidBlockHeight: 99n };
    },
    async simulateTransaction() {
      return { err: null, logs: [], unitsConsumed: 1n };
    },
    async sendRawTransaction() {
      signatureIndex += 1;
      return `signature-${signatureIndex}`;
    },
    async confirmTransaction(signature) {
      return { signature, slot: BigInt(signatureIndex), confirmationStatus: "confirmed" };
    },
  };
  const owner = "11111111111111111111111111111111";
  const encrypted = new Uint8Array(64);
  encrypted[0] = 1;
  encrypted[32] = 1;
  const randomness = new Uint8Array(32);
  randomness[0] = 9;
  const client = createBeatExClient({
    rpc,
    signer: { address: owner, async signTransaction(bytes) { return bytes; } },
    prover: {
      async proveDeposit(request) {
        assert.equal(request.oldRevision, 0n);
        return {
          proof: {
            proofA: new Uint8Array(64),
            proofB: new Uint8Array(128),
            proofC: new Uint8Array(64),
            publicInput: new Uint8Array(32),
          },
          newEncryptedBase: encrypted,
          newEncryptedQuote: encrypted,
          newBalance: {
            baseValue: request.baseAmount,
            quoteValue: request.quoteAmount,
            baseRandomness: randomness,
            quoteRandomness: randomness,
          },
        };
      },
    },
    storage: new InMemoryPrivateStateStorage(),
    transactionCompiler: {
      async compileTransaction(request) {
        sent.push(request);
        return new Uint8Array([1, 2, 3]);
      },
    },
    cluster: "devnet",
  });
  const result = await client.deposit({
    market: MARKET,
    baseSource: OWNER,
    quoteSource: OWNER,
    baseAmount: 100n,
    quoteAmount: 10_000n,
  });
  assert.equal(sent.length, 2);
  assert.equal(sent[0].instructions.length, 1);
  assert.equal(sent[0].instructions[0].accounts.length, 5);
  assert.equal(sent[1].instructions[0].accounts.length, 13);
  assert.equal(sent[1].instructions[0].data.length, 440);
  assert.equal(result.privateState.state.balanceRevision, 1n);
  assert.equal(result.privateState.state.balance.quoteValue, 10_000n);
  assert.equal(result.initializationTransaction.signature, "signature-1");
  assert.equal(result.transaction.signature, "signature-2");
});
