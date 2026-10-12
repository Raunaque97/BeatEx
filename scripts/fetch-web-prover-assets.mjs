import { createHash } from "node:crypto";
import {
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("../", import.meta.url));
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

export function verifyAsset(bytes, descriptor) {
  if (
    bytes.length !== descriptor.byteLength ||
    sha256(bytes) !== descriptor.sha256
  ) {
    throw new Error(`Integrity check failed for ${descriptor.url}`);
  }
}

async function download(url, maxBytes) {
  const response = await fetch(url, { signal: AbortSignal.timeout(60_000) });
  if (!response.ok)
    throw new Error(`Downloading ${url} returned HTTP ${response.status}`);
  const chunks = [];
  let length = 0;
  for await (const chunk of response.body) {
    length += chunk.length;
    if (length > maxBytes)
      throw new Error(`Response exceeds expected size: ${url}`);
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

export async function fetchProverAssets(config, output) {
  if (!/^[a-f0-9]{64}$/.test(config.manifestSha256)) {
    throw new Error("The release configuration must pin a manifest SHA-256.");
  }
  let manifestBytes;
  try {
    manifestBytes = await readFile(join(output, "manifest.json"));
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (!manifestBytes || sha256(manifestBytes) !== config.manifestSha256) {
    manifestBytes = await download(config.manifestUrl, 1_000_000);
  }
  if (sha256(manifestBytes) !== config.manifestSha256) {
    throw new Error("Release manifest does not match its pinned SHA-256.");
  }
  const manifest = JSON.parse(manifestBytes);
  if (
    manifest.schemaVersion !== 1 ||
    manifest.backend !== "beatex-arkworks-groth16-wasm-v2"
  ) {
    throw new Error("Unsupported prover manifest.");
  }
  const assets = [
    manifest.runtime,
    ...["balance", "order", "release", "settlement"].map(
      (name) => manifest.circuits[name].provingKey,
    ),
  ];
  const names = new Set();
  for (const asset of assets) {
    if (
      !/^[a-zA-Z0-9_.-]+\.(wasm|pk)$/.test(asset.url) ||
      names.has(asset.url) ||
      !Number.isSafeInteger(asset.byteLength) ||
      asset.byteLength <= 0 ||
      asset.byteLength > 100_000_000 ||
      !/^[a-f0-9]{64}$/.test(asset.sha256)
    )
      throw new Error("Invalid asset descriptor in prover manifest.");
    names.add(asset.url);
  }
  // Verify every file before replacing the existing public assets.
  const bytes = await Promise.all(
    assets.map(async (asset) => {
      let data;
      try {
        data = await readFile(join(output, asset.url));
        verifyAsset(data, asset);
      } catch (error) {
        if (error.code && error.code !== "ENOENT") throw error;
        data = await download(
          new URL(asset.url, config.manifestUrl),
          asset.byteLength,
        );
        verifyAsset(data, asset);
      }
      return data;
    }),
  );
  await mkdir(dirname(output), { recursive: true });
  const staging = await mkdtemp(join(dirname(output), ".prover-assets-"));
  try {
    await Promise.all(
      assets.map((asset, index) =>
        writeFile(join(staging, asset.url), bytes[index]),
      ),
    );
    await writeFile(join(staging, "manifest.json"), manifestBytes);
    await rm(output, { recursive: true, force: true });
    await rename(staging, output);
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
  console.log(
    `Verified ${assets.length} prover binaries and manifest in ${output}`,
  );
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const config = JSON.parse(
    await readFile(join(repoRoot, "apps/web/prover-assets.release.json")),
  );
  await fetchProverAssets(
    config,
    join(repoRoot, "apps/web/public/beatex-prover-assets"),
  );
}
