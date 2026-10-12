import { GENERATED_DEPLOYMENT } from "@beatex/sdk";
import { NextResponse } from "next/server";

import { MARKETS } from "@/lib/config/markets";

import { FaucetError, dripTestFunds, faucetConfigFromEnv } from "./faucet";

export const runtime = "nodejs";

export async function POST(request: Request) {
  try {
    const body = (await request.json().catch(() => null)) as {
      owner?: unknown;
    } | null;
    if (typeof body?.owner !== "string") {
      return NextResponse.json(
        { error: "Send { owner: <wallet address> }." },
        { status: 400 },
      );
    }
    // Only the tBTC/tUSD market's tokens are dispensed; BEAT has no faucet.
    const market = MARKETS.find((candidate) => candidate.faucet);
    if (market === undefined) {
      throw new FaucetError("No faucet market is configured.", 503);
    }
    const config = faucetConfigFromEnv(
      market,
      GENERATED_DEPLOYMENT.baseMint,
      GENERATED_DEPLOYMENT.quoteMint,
    );
    const result = await dripTestFunds(config, body.owner);
    return NextResponse.json({
      signature: result.signature,
      baseAtoms: result.baseAtoms.toString(),
      quoteAtoms: result.quoteAtoms.toString(),
      lamports: result.lamports.toString(),
    });
  } catch (error) {
    if (error instanceof FaucetError) {
      return NextResponse.json(
        { error: error.message },
        { status: error.status },
      );
    }
    console.error("faucet failed", error);
    return NextResponse.json(
      { error: "The faucet transaction failed. Please retry." },
      { status: 502 },
    );
  }
}
