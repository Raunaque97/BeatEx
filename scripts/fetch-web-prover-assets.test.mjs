import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { fetchProverAssets } from "./fetch-web-prover-assets.mjs";

async function fixture(t, corruption) {
  const files = new Map();
  const descriptor = (name) => {
    const bytes = Buffer.from(name);
    files.set(`/${name}`, bytes);
    return {
      url: name,
      byteLength: bytes.length,
      sha256: createHash("sha256").update(bytes).digest("hex"),
    };
  };
  const manifest = {
    schemaVersion: 1,
    backend: "beatex-arkworks-groth16-wasm-v2",
    runtime: descriptor("prover.wasm"),
    circuits: Object.fromEntries(
      ["balance", "order", "release", "settlement"].map((name) => [
        name,
        { provingKey: descriptor(`${name}.pk`) },
      ]),
    ),
  };
  const body = Buffer.from(JSON.stringify(manifest));
  files.set("/manifest.json", body);
  if (corruption === "hash") files.set("/order.pk", Buffer.from("xxxxxxxx"));
  if (corruption === "oversize") files.set("/order.pk", Buffer.alloc(100));
  const server = createServer((request, response) => {
    const bytes = files.get(request.url);
    response.writeHead(bytes ? 200 : 404);
    response.end(bytes);
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const output = await mkdtemp(join(tmpdir(), "beatex-assets-test-"));
  t.after(async () => {
    await new Promise((resolve) => server.close(resolve));
    await rm(output, { recursive: true, force: true });
  });
  return {
    output,
    config: {
      manifestUrl: `http://127.0.0.1:${server.address().port}/manifest.json`,
      manifestSha256: createHash("sha256").update(body).digest("hex"),
    },
  };
}

test("downloads all assets from a clean build and preserves the pinned manifest", async (t) => {
  const { output, config } = await fixture(t);
  await fetchProverAssets(config, output);
  for (const name of [
    "prover.wasm",
    "balance.pk",
    "order.pk",
    "release.pk",
    "settlement.pk",
  ]) {
    assert.equal(await readFile(join(output, name), "utf8"), name);
  }
  assert.equal(
    createHash("sha256")
      .update(await readFile(join(output, "manifest.json")))
      .digest("hex"),
    config.manifestSha256,
  );
});

for (const corruption of ["hash", "oversize"]) {
  test(`rejects ${corruption} corruption without replacing existing assets`, async (t) => {
    const { output, config } = await fixture(t, corruption);
    await writeFile(join(output, "existing.txt"), "keep me");
    await assert.rejects(
      fetchProverAssets(config, output),
      /Integrity check failed|Response exceeds expected size/,
    );
    assert.equal(
      await readFile(join(output, "existing.txt"), "utf8"),
      "keep me",
    );
    await assert.rejects(readFile(join(output, "manifest.json")), {
      code: "ENOENT",
    });
  });
}

test("rejects a release whose manifest differs from the committed digest", async (t) => {
  const { output, config } = await fixture(t);
  config.manifestSha256 = "0".repeat(64);
  await assert.rejects(
    fetchProverAssets(config, output),
    /manifest does not match/,
  );
});
