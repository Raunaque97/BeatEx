/**
 * Below this many lamports the faucet tops a wallet back up. It covers the
 * private-balance rent (~0.0021 SOL), one order's account rent (~0.0146 SOL),
 * and transaction fees.
 */
export const SOL_TOP_UP_FLOOR_LAMPORTS = 20_000_000n;
