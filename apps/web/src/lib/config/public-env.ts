import { z } from "zod";
import { GENERATED_PROGRAM_ID } from "@beatex/sdk";

const publicEnvSchema = z.object({
  cluster: z.enum(["localnet", "devnet"]).default("devnet"),
  rpcUrl: z.url().default("https://api.devnet.solana.com"),
  wsUrl: z.url().default("wss://api.devnet.solana.com"),
  programId: z.string().min(32).default(GENERATED_PROGRAM_ID),
  proverManifestUrl: z
    .string()
    .min(1)
    .default("/beatex-prover-assets/manifest.json"),
  /** The oracle-free BEAT/tUSD market, listed when configured. */
  beatMarketId: z.string().min(32).optional(),
  /** Auction-history indexer (scripts/devnet/run-indexer.sh). */
  indexerUrl: z.url().default("http://127.0.0.1:8787"),
});

export const publicEnv = publicEnvSchema.parse({
  cluster: process.env.NEXT_PUBLIC_BEATEX_CLUSTER,
  rpcUrl: process.env.NEXT_PUBLIC_SOLANA_RPC_URL,
  wsUrl: process.env.NEXT_PUBLIC_SOLANA_WS_URL,
  programId: process.env.NEXT_PUBLIC_BEATEX_PROGRAM_ID,
  proverManifestUrl: process.env.NEXT_PUBLIC_BEATEX_PROVER_MANIFEST_URL,
  indexerUrl: process.env.NEXT_PUBLIC_BEATEX_INDEXER_URL,
  beatMarketId: process.env.NEXT_PUBLIC_BEATEX_BEAT_MARKET || undefined,
});

export type PublicEnv = typeof publicEnv;
