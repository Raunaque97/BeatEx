import { z } from "zod";

const publicEnvSchema = z.object({
  cluster: z.enum(["localnet", "devnet"]).default("devnet"),
  rpcUrl: z.url().default("https://api.devnet.solana.com"),
  wsUrl: z.url().default("wss://api.devnet.solana.com"),
  programId: z
    .string()
    .min(32)
    .default("2P37xv7CzDv7pSet6LaXJDq4m1auW3pZ8PzPLtErfsVd"),
  proverManifestUrl: z
    .string()
    .min(1)
    .default("/beatex-prover-assets/manifest.json"),
});

export const publicEnv = publicEnvSchema.parse({
  cluster: process.env.NEXT_PUBLIC_BEATEX_CLUSTER,
  rpcUrl: process.env.NEXT_PUBLIC_SOLANA_RPC_URL,
  wsUrl: process.env.NEXT_PUBLIC_SOLANA_WS_URL,
  programId: process.env.NEXT_PUBLIC_BEATEX_PROGRAM_ID,
  proverManifestUrl: process.env.NEXT_PUBLIC_BEATEX_PROVER_MANIFEST_URL,
});

export type PublicEnv = typeof publicEnv;
