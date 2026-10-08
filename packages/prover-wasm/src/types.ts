import type { ProofCancellationSignal, ProvingProgress } from "@beatex/sdk";

export type TraderCircuit = "balance" | "order" | "release" | "settlement";

export type TraderProofOperation =
  | "deposit"
  | "withdrawal"
  | "order"
  | "release"
  | "settlement"
  | "replacement"
  | "rollover";

/** A content-addressed browser asset. sha256 is a lowercase, 64-character hex digest. */
export interface ProverAssetDescriptor {
  readonly url: string;
  readonly byteLength: number;
  readonly sha256: string;
}

export interface CircuitAssetDescriptor {
  readonly circuitVersion: number;
  readonly provingKey: ProverAssetDescriptor;
}

/**
 * Deploy this manifest next to the generated runtime and proving keys. URLs may
 * be absolute or relative to `assetBaseUrl`.
 */
export interface ProverAssetManifest {
  readonly schemaVersion: 1;
  readonly backend: string;
  readonly provingKeyEncoding: "arkworks-canonical-uncompressed-v1";
  readonly runtime: ProverAssetDescriptor;
  readonly circuits: Readonly<Record<TraderCircuit, CircuitAssetDescriptor>>;
}

export interface ProverAssetCache {
  get(key: string): Promise<ArrayBuffer | undefined>;
  put(key: string, value: ArrayBuffer): Promise<void>;
  delete?(key: string): Promise<void>;
}

export interface ProverAssetTransport {
  load(url: string, signal?: ProofCancellationSignal): Promise<ArrayBuffer>;
}

export interface WorkerMessageEventLike {
  readonly data: unknown;
}

export interface WorkerErrorEventLike {
  readonly error?: unknown;
  readonly message?: string;
}

/** Small structural subset shared by browser Workers and Node test doubles. */
export interface ProverWorker {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  addEventListener(type: "message", listener: (event: WorkerMessageEventLike) => void): void;
  addEventListener(type: "error", listener: (event: WorkerErrorEventLike) => void): void;
  removeEventListener(type: "message", listener: (event: WorkerMessageEventLike) => void): void;
  removeEventListener(type: "error", listener: (event: WorkerErrorEventLike) => void): void;
  terminate(): void;
}

export type ProverWorkerFactory = () => ProverWorker;

export interface WasmBeatExProverOptions {
  readonly manifest: ProverAssetManifest;
  readonly assetBaseUrl?: string | URL;
  readonly assetTransport?: ProverAssetTransport;
  readonly assetCache?: ProverAssetCache;
  readonly workerFactory?: ProverWorkerFactory;
  /** Override only when hosting a separately bundled copy of the package worker. */
  readonly workerUrl?: string | URL;
}

export interface WorkerInitializeRequest {
  readonly type: "initialize";
  readonly id: number;
  readonly runtime: ArrayBuffer;
}

export interface WorkerLoadRequest {
  readonly type: "load";
  readonly id: number;
  readonly circuit: TraderCircuit;
  readonly backend: string;
  readonly circuitVersion: number;
  readonly provingKey: ArrayBuffer;
}

export interface WorkerProveRequest {
  readonly type: "prove";
  readonly id: number;
  readonly circuit: TraderCircuit;
  readonly operation: TraderProofOperation;
  readonly request: unknown;
}

export interface WorkerCancelRequest {
  readonly type: "cancel";
  readonly id: number;
}

export interface WorkerDisposeRequest {
  readonly type: "dispose";
}

export type MainToWorkerMessage =
  | WorkerInitializeRequest
  | WorkerLoadRequest
  | WorkerProveRequest
  | WorkerCancelRequest
  | WorkerDisposeRequest;

export interface WorkerSuccessResponse {
  readonly type: "result";
  readonly id: number;
  readonly result: unknown;
}

export interface WorkerErrorResponse {
  readonly type: "error";
  readonly id: number;
  readonly error: { readonly name: string; readonly message: string; readonly stack?: string };
}

export interface WorkerProgressResponse {
  readonly type: "progress";
  readonly id: number;
  readonly progress: ProvingProgress;
}

export type WorkerToMainMessage =
  | WorkerSuccessResponse
  | WorkerErrorResponse
  | WorkerProgressResponse;
