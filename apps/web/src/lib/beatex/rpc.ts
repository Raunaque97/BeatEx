import { createBeatExRpc } from "@beatex/sdk";

import { publicEnv } from "@/lib/config/public-env";

/** Shared read/write RPC for the configured cluster. */
export const beatexRpc = createBeatExRpc(publicEnv.rpcUrl);
