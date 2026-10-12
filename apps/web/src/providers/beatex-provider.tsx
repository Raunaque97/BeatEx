"use client";

import {
  BrowserPrivateStateStorage,
  createBeatExClient,
  createKitSignerAdapter,
  type BeatExClient,
  type BeatExProver,
  type ProofExecutionOptions,
} from "@beatex/sdk";
import { signBytes, type KeyPairSigner } from "@solana/kit";
import { useQueryClient } from "@tanstack/react-query";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import { deleteDemoWallet, loadDemoWallet } from "@/lib/beatex/demo-wallet";
import { getBrowserProver } from "@/lib/beatex/prover";
import { beatexRpc } from "@/lib/beatex/rpc";
import {
  EncryptedStateStore,
  deriveStateKey,
  indexedDbStore,
  stateKeyMessage,
} from "@/lib/beatex/state-encryption";
import { TxTracker, type TxProgress } from "@/lib/beatex/tx-tracker";
import { publicEnv } from "@/lib/config/public-env";
import { queryKeys } from "@/lib/query/keys";

interface DemoWallet {
  readonly signer: KeyPairSigner;
  /** Decrypts private state; derived from the wallet's own signature. */
  readonly stateKey: CryptoKey;
}

async function openDemoWallet(): Promise<DemoWallet> {
  const signer = await loadDemoWallet();
  const scope = {
    cluster: publicEnv.cluster,
    programId: publicEnv.programId,
    owner: signer.address,
  };
  // Ed25519 signatures are deterministic, so the same wallet always derives the same key.
  const signature = await signBytes(
    signer.keyPair.privateKey,
    stateKeyMessage(scope),
  );
  return { signer, stateKey: await deriveStateKey(signature, scope) };
}

/** Resolves the shared WASM prover on first use so no proving assets load before a proof. */
const lazyProver = new Proxy({} as BeatExProver, {
  get:
    (_target, method: string) =>
    async (...args: unknown[]) => {
      const prover = (await getBrowserProver()) as unknown as Record<
        string,
        (...inner: unknown[]) => Promise<unknown>
      >;
      return prover[method]!(...args);
    },
});

export interface BeatExSession {
  /** The demo wallet's address; null while it loads. */
  readonly owner: string | null;
  /** Why the demo wallet could not be opened, if it could not. */
  readonly walletError: string | null;
  /** Ready once the demo wallet has loaded. */
  readonly client: BeatExClient | null;
  readonly tracker: TxTracker;
  /** Bumped after every confirmed action so private views reload. */
  readonly privateRevision: number;
  readonly notifyPrivateChange: () => void;
  /** Replace this browser's demo wallet with a fresh one. */
  readonly resetWallet: () => Promise<void>;
}

const SessionContext = createContext<BeatExSession | null>(null);

export function BeatExProvider({ children }: { readonly children: ReactNode }) {
  const [tracker] = useState(() => new TxTracker());
  const [wallet, setWallet] = useState<DemoWallet | null>(null);
  const [walletError, setWalletError] = useState<string | null>(null);
  const [privateRevision, setPrivateRevision] = useState(0);

  useEffect(() => {
    let cancelled = false;
    openDemoWallet().then(
      (opened) => {
        if (!cancelled) setWallet(opened);
      },
      (error: unknown) => {
        if (!cancelled)
          setWalletError(
            error instanceof Error
              ? `This browser couldn't create a demo wallet: ${error.message}`
              : "This browser couldn't create a demo wallet.",
          );
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const client = useMemo(() => {
    if (wallet === null) return null;
    return createBeatExClient({
      rpc: tracker.rpc(beatexRpc),
      signer: tracker.signer(createKitSignerAdapter(wallet.signer)),
      prover: lazyProver,
      storage: new BrowserPrivateStateStorage(
        new EncryptedStateStore(wallet.stateKey, indexedDbStore()),
      ),
      cluster: publicEnv.cluster,
      programId: publicEnv.programId,
    });
  }, [wallet, tracker]);

  const notifyPrivateChange = useCallback(
    () => setPrivateRevision((value) => value + 1),
    [],
  );

  const resetWallet = useCallback(async () => {
    setWallet(null);
    await deleteDemoWallet();
    setWallet(await openDemoWallet());
    setPrivateRevision((value) => value + 1);
  }, []);

  const session = useMemo<BeatExSession>(
    () => ({
      owner: wallet?.signer.address ?? null,
      walletError,
      client,
      tracker,
      privateRevision,
      notifyPrivateChange,
      resetWallet,
    }),
    [
      wallet,
      walletError,
      client,
      tracker,
      privateRevision,
      notifyPrivateChange,
      resetWallet,
    ],
  );

  return (
    <SessionContext.Provider value={session}>
      {children}
    </SessionContext.Provider>
  );
}

export function useBeatEx(): BeatExSession {
  const session = useContext(SessionContext);
  if (session === null)
    throw new Error("useBeatEx must be used inside BeatExProvider");
  return session;
}

/**
 * Run one SDK action with phase reporting. On success the private views reload and
 * public wallet balances are refetched.
 */
export function useTrackedAction() {
  const { tracker, notifyPrivateChange, owner } = useBeatEx();
  const queryClient = useQueryClient();
  const [progress, setProgress] = useState<TxProgress>({ phase: "idle" });

  const run = useCallback(
    async <T,>(action: (proofOptions: ProofExecutionOptions) => Promise<T>) => {
      try {
        return await tracker.run(setProgress, action);
      } finally {
        notifyPrivateChange();
        if (owner !== null) {
          void queryClient.invalidateQueries({
            queryKey: queryKeys.walletBalances(owner),
          });
        }
      }
    },
    [tracker, notifyPrivateChange, queryClient, owner],
  );
  const reset = useCallback(() => setProgress({ phase: "idle" }), []);
  const busy =
    progress.phase !== "idle" &&
    progress.phase !== "confirmed" &&
    progress.phase !== "failed";

  return { progress, run, reset, busy, setProgress };
}
