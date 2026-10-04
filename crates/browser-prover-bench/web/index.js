import init, {
  BrowserProver,
  ristrettoConstraintReport,
} from "./pkg/beatex_browser_prover_bench.js";

const output = document.querySelector("#output");
const runButton = document.querySelector("#run");
const ristrettoButton = document.querySelector("#measure-ristretto");
const downloadButton = document.querySelector("#download");
const levelsInput = document.querySelector("#levels");
const roundsInput = document.querySelector("#rounds");
const query = new URLSearchParams(location.search);
let lastReport;

if (query.has("levels")) levelsInput.value = query.get("levels");
if (query.has("rounds")) roundsInput.value = query.get("rounds");

const wasm = await init();
output.textContent = "Ready.";

const memoryBytes = () => wasm.memory.buffer.byteLength;
const stats = (samples) => {
  const sorted = [...samples].sort((a, b) => a - b);
  const sum = sorted.reduce((total, value) => total + value, 0);
  return {
    min: sorted[0],
    median: sorted[Math.floor(sorted.length / 2)],
    mean: sum / sorted.length,
    max: sorted.at(-1),
  };
};

async function run() {
  const levels = Number(levelsInput.value);
  const rounds = Number(roundsInput.value);
  runButton.disabled = true;
  downloadButton.disabled = true;
  output.textContent = `Generating deterministic ${levels}-level proving key in WASM…`;
  await new Promise(requestAnimationFrame);

  try {
    const beforeSetup = memoryBytes();
    const wallSetupStarted = performance.now();
    const prover = new BrowserProver(levels);
    const wallSetupMs = performance.now() - wallSetupStarted;
    const afterSetup = memoryBytes();
    output.textContent = `Generating and verifying ${rounds} proof(s) in WASM…`;
    await new Promise(requestAnimationFrame);
    const benchmark = prover.benchmark(rounds);
    const afterProofs = memoryBytes();

    lastReport = {
      userAgent: navigator.userAgent,
      crossOriginIsolated,
      generatedAt: new Date().toISOString(),
      setup: { ...prover.setupReport(), wallSetupMs },
      benchmark: {
        ...benchmark,
        proofStatsMs: stats(benchmark.proofMs),
        exportStatsMs: stats(benchmark.exportMs),
        verifyStatsMs: stats(benchmark.verifyMs),
      },
      wasmMemoryBytes: { beforeSetup, afterSetup, afterProofs },
      caveats: [
        "WASM memory is committed linear memory, not process RSS or live heap.",
        "Linear memory grows in 64 KiB pages and does not shrink.",
        "The circuit excludes Ristretto ciphertext binding.",
        "Deterministic seeds are benchmark-only and not deployment security.",
      ],
    };
    output.textContent = JSON.stringify(lastReport, null, 2);
    downloadButton.disabled = false;
    prover.free();
  } catch (error) {
    output.textContent = error?.stack ?? String(error);
  } finally {
    runButton.disabled = false;
  }
}

async function measureRistretto() {
  const levels = Number(levelsInput.value);
  runButton.disabled = true;
  ristrettoButton.disabled = true;
  downloadButton.disabled = true;
  output.textContent = "Synthesizing one exact Ristretto ciphertext relation in WASM…";
  await new Promise(requestAnimationFrame);

  try {
    const before = memoryBytes();
    const started = performance.now();
    const shape = ristrettoConstraintReport(levels * 2);
    const synthesisMs = performance.now() - started;
    const after = memoryBytes();
    lastReport = {
      userAgent: navigator.userAgent,
      generatedAt: new Date().toISOString(),
      ristretto: { ...shape, synthesisMs },
      wasmMemoryBytes: { before, after },
      caveats: [
        "The function synthesizes zero- and one-binding circuits, then extrapolates the exact linear shape.",
        "No Groth16 setup or proof is attempted for the multi-million-constraint relation.",
      ],
    };
    output.textContent = JSON.stringify(lastReport, null, 2);
    downloadButton.disabled = false;
  } catch (error) {
    output.textContent = error?.stack ?? String(error);
  } finally {
    runButton.disabled = false;
    ristrettoButton.disabled = false;
  }
}

runButton.addEventListener("click", run);
ristrettoButton.addEventListener("click", measureRistretto);
downloadButton.addEventListener("click", () => {
  const blob = new Blob([JSON.stringify(lastReport, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  const levels = lastReport.setup?.levels ?? lastReport.ristretto.bindings / 2;
  link.download = `beatex-groth16-wasm-${levels}-levels.json`;
  link.click();
  URL.revokeObjectURL(url);
});

if (query.get("autorun") === "1") run();
