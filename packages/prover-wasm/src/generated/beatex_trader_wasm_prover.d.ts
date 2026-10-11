/* tslint:disable */
/* eslint-disable */
/**
 * ABI version for worker/package compatibility checks.
 */
export function traderProverApiVersion(): number;
/**
 * One in-memory runtime for every proof needed by a trader frontend.
 *
 * Key bytes must be SHA-256-authenticated Arkworks canonical-uncompressed
 * `ProvingKey<Bn254>` artifacts for circuit version 2. Each artifact is
 * decoded exactly once. The official TypeScript loader performs the required
 * digest and byte-length checks before calling this lower-level API.
 */
export class TraderProver {
  free(): void;
  constructor();
  /**
   * Decode and retain one already-authenticated proving key. Setup never
   * runs in the browser.
   */
  loadProvingKey(circuit: string, bytes: Uint8Array): void;
  loadProvingKeyForVersion(circuit: string, circuit_version: number, bytes: Uint8Array): void;
  isProvingKeyLoaded(circuit: string): boolean;
  proveDeposit(request: any): any;
  proveWithdrawal(request: any): any;
  proveOrder(request: any): any;
  proveCancellation(request: any): any;
  proveSettlement(request: any): any;
  proveReplacement(request: any): any;
  proveRollover(request: any): any;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly __wbg_traderprover_free: (a: number, b: number) => void;
  readonly traderprover_new: () => number;
  readonly traderprover_loadProvingKey: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
  readonly traderprover_loadProvingKeyForVersion: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
  readonly traderprover_isProvingKeyLoaded: (a: number, b: number, c: number) => number;
  readonly traderprover_proveDeposit: (a: number, b: number, c: number) => void;
  readonly traderprover_proveWithdrawal: (a: number, b: number, c: number) => void;
  readonly traderprover_proveOrder: (a: number, b: number, c: number) => void;
  readonly traderprover_proveCancellation: (a: number, b: number, c: number) => void;
  readonly traderprover_proveSettlement: (a: number, b: number, c: number) => void;
  readonly traderprover_proveReplacement: (a: number, b: number, c: number) => void;
  readonly traderprover_proveRollover: (a: number, b: number, c: number) => void;
  readonly traderProverApiVersion: () => number;
  readonly __wbindgen_export_0: (a: number, b: number) => number;
  readonly __wbindgen_export_1: (a: number, b: number, c: number, d: number) => number;
  readonly __wbindgen_export_2: (a: number) => void;
  readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;
/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
*
* @returns {InitOutput}
*/
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
