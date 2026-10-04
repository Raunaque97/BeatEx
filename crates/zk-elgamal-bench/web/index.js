import init, {
  BatchedGroupedCiphertext2HandlesValidityProofData,
  BatchedRangeProofU64Data,
  ElGamalKeypair,
  GroupedElGamalCiphertext2Handles,
  PedersenCommitment,
  PedersenOpening,
} from "@solana/zk-sdk/web";

const QUANTITY_BITS = 4;
const MAX_RANGE_VALUES = 7;
const levelsToTest = [3, 9, 17, 33, 65];
const output = document.querySelector("#output");
const runButton = document.querySelector("#run");
const roundsInput = document.querySelector("#rounds");

await init();
output.textContent = "Ready.";

const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];

function deterministicQuantities(levels) {
  const quantities = [];
  const middle = Math.floor(levels / 2);
  for (let level = 0; level < levels; level += 1) {
    const quantity = BigInt((level * 7 + 3) % 15 + 1);
    if (level < middle) quantities.push(quantity, 0n);
    else if (level > middle) quantities.push(0n, quantity);
    else quantities.push(0n, 0n);
  }
  return quantities;
}

function benchmarkOrder(levels, committee, recovery) {
  const amounts = deterministicQuantities(levels);
  const openings = [];
  const ciphertexts = [];
  const commitments = [];

  let started = performance.now();
  for (const amount of amounts) {
    const opening = new PedersenOpening();
    const ciphertext = GroupedElGamalCiphertext2Handles.encryptWith(
      committee,
      recovery,
      amount,
      opening,
    );
    const commitment = PedersenCommitment.fromBytes(ciphertext.toBytes().slice(0, 32));
    openings.push(opening);
    ciphertexts.push(ciphertext);
    commitments.push(commitment);
  }
  const encryptionMs = performance.now() - started;

  started = performance.now();
  const validityProofs = [];
  for (let index = 0; index < amounts.length; index += 2) {
    validityProofs.push(
      new BatchedGroupedCiphertext2HandlesValidityProofData(
        committee,
        recovery,
        ciphertexts[index],
        ciphertexts[index + 1],
        amounts[index],
        amounts[index + 1],
        openings[index],
        openings[index + 1],
      ),
    );
  }
  const validityProveMs = performance.now() - started;

  started = performance.now();
  const rangeProofs = [];
  for (let offset = 0; offset < amounts.length; offset += MAX_RANGE_VALUES) {
    const end = Math.min(offset + MAX_RANGE_VALUES, amounts.length);
    const batchAmounts = amounts.slice(offset, end);
    const batchOpenings = openings.slice(offset, end);
    const batchCommitments = commitments.slice(offset, end);
    const paddingOpening = new PedersenOpening();
    const paddingCommitment = PedersenCommitment.from(0n, paddingOpening);
    batchAmounts.push(0n);
    batchOpenings.push(paddingOpening);
    batchCommitments.push(paddingCommitment);
    const bitLengths = new Uint8Array(batchAmounts.length).fill(QUANTITY_BITS);
    bitLengths[bitLengths.length - 1] = 64 - QUANTITY_BITS * (batchAmounts.length - 1);
    rangeProofs.push(
      new BatchedRangeProofU64Data(
        batchCommitments,
        new BigUint64Array(batchAmounts),
        bitLengths,
        batchOpenings,
      ),
    );
  }
  const rangeProveMs = performance.now() - started;

  started = performance.now();
  for (const proof of validityProofs) proof.verify();
  for (const proof of rangeProofs) proof.verify();
  const verifyMs = performance.now() - started;

  const proofDataBytes = validityProofs.reduce((sum, proof) => sum + proof.toBytes().length, 0)
    + rangeProofs.reduce((sum, proof) => sum + proof.toBytes().length, 0);

    // BatchedRangeProofU64Data takes ownership of commitments and openings.
    // Their WASM wrappers have null pointers after construction, so freeing them
    // here would double-free the underlying Rust values.
    for (const value of [...validityProofs, ...rangeProofs, ...ciphertexts]) {
      value.free();
    }

  return {
    encryptionMs,
    validityProveMs,
    rangeProveMs,
    verifyMs,
    validityProofs: levels,
    rangeProofs: Math.ceil((levels * 2) / MAX_RANGE_VALUES),
    proofDataBytes,
  };
}

async function run() {
  const rounds = Number(roundsInput.value);
  runButton.disabled = true;
  output.textContent = "Generating proofs in browser WASM…";
  await new Promise(requestAnimationFrame);

  try {
    const committeeKeypair = new ElGamalKeypair();
    const recoveryKeypair = new ElGamalKeypair();
    const committee = committeeKeypair.pubkey();
    const recovery = recoveryKeypair.pubkey();
    const results = [];

    for (const levels of levelsToTest) {
      const samples = [];
      for (let round = 0; round < rounds; round += 1) {
        samples.push(benchmarkOrder(levels, committee, recovery));
        await new Promise(requestAnimationFrame);
      }
      const first = samples[0];
      results.push({
        levels,
        quantities: levels * 2,
        validityProofs: first.validityProofs,
        rangeProofs: first.rangeProofs,
        proofDataBytes: first.proofDataBytes,
        medianEncryptionMs: median(samples.map((sample) => sample.encryptionMs)),
        medianValidityProveMs: median(samples.map((sample) => sample.validityProveMs)),
        medianRangeProveMs: median(samples.map((sample) => sample.rangeProveMs)),
        medianHostVerifyMs: median(samples.map((sample) => sample.verifyMs)),
      });
      output.textContent = JSON.stringify({ completed: levels, results }, null, 2);
    }

    recovery.free();
    committee.free();
    recoveryKeypair.free();
    committeeKeypair.free();
    output.textContent = JSON.stringify({
      generatedAt: new Date().toISOString(),
      userAgent: navigator.userAgent,
      rounds,
      results,
    }, null, 2);
  } catch (error) {
    output.textContent = error?.stack ?? String(error);
  } finally {
    runButton.disabled = false;
  }
}

runButton.addEventListener("click", run);
if (new URLSearchParams(location.search).get("autorun") === "1") run();
