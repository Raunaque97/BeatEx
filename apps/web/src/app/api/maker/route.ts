import { createWasmBeatExProver } from "@beatex/prover-wasm";
import { createInProcessProverWorker } from "@beatex/prover-wasm/in-process";
import type { ProverAssetManifest } from "@beatex/prover-wasm";
import type { BeatExProver } from "@beatex/sdk";
import { after, NextResponse } from "next/server";

import { findMarket } from "@/lib/config/markets";
import { publicEnv } from "@/lib/config/public-env";

import {
  MakerError,
  isTrader,
  makerConfigFromEnv,
  openCurrentAuction,
  runMaker,
  targetAuction,
} from "./maker";

export const runtime = "nodejs";
// One call quotes, waits for the auction to clear, and settles.
export const maxDuration = 300;

let shared: Promise<BeatExProver> | undefined;

/** One prover per warm instance, so its keys stay loaded between calls. */
function serverProver(origin: string): Promise<BeatExProver> {
  shared ??= (async () => {
    const manifestUrl = new URL(publicEnv.proverManifestUrl, origin);
    const response = await fetch(manifestUrl);
    if (!response.ok) {
      throw new Error(`Prover manifest returned HTTP ${response.status}`);
    }
    return createWasmBeatExProver({
      manifest: (await response.json()) as ProverAssetManifest,
      assetBaseUrl: new URL("./", manifestUrl),
      workerFactory: createInProcessProverWorker,
    });
  })();
  shared.catch(() => {
    shared = undefined;
  });
  return shared;
}

export async function POST(request: Request) {
  try {
    const body = (await request.json().catch(() => null)) as {
      market?: unknown;
      owner?: unknown;
      open?: unknown;
    } | null;
    if (typeof body?.market !== "string" || typeof body.owner !== "string") {
      return NextResponse.json(
        { error: "Send { market: <slug>, owner: <wallet address> }." },
        { status: 400 },
      );
    }
    const market = findMarket(body.market);
    if (market === undefined) {
      throw new MakerError("Unknown market.", 404);
    }
    const config = makerConfigFromEnv(market);
    if (!(await isTrader(config, body.owner))) {
      throw new MakerError("Deposit before trading.", 403);
    }

    if (body.open === true) await openCurrentAuction(config);
    const auctionId = await targetAuction(config);
    const { origin } = new URL(request.url);
    after(async () => {
      const log = (step: string, detail?: unknown) =>
        console.info(
          `maker ${market.slug} ${step}`,
          JSON.stringify(detail ?? {}, (_, value: unknown) =>
            typeof value === "bigint" ? value.toString() : value,
          ),
        );
      try {
        await runMaker(config, await serverProver(origin), auctionId, log);
      } catch (error) {
        console.error(`maker ${market.slug} failed`, error);
      }
    });
    return NextResponse.json(
      { auctionId: auctionId.toString() },
      { status: 202 },
    );
  } catch (error) {
    if (error instanceof MakerError) {
      return NextResponse.json(
        { error: error.message },
        { status: error.status },
      );
    }
    console.error("maker failed", error);
    return NextResponse.json(
      { error: "The maker could not start." },
      { status: 502 },
    );
  }
}
