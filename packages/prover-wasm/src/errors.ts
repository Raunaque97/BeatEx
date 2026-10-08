export class ProverAssetIntegrityError extends Error {
  readonly url: string;

  constructor(url: string, message: string) {
    super(`Prover asset integrity check failed for ${url}: ${message}`);
    this.name = "ProverAssetIntegrityError";
    this.url = url;
  }
}

export class ProverWorkerError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "ProverWorkerError";
  }
}

export function abortError(reason?: unknown): Error {
  if (reason instanceof Error) return reason;
  return new DOMException(reason === undefined ? "Proof generation was aborted" : String(reason), "AbortError");
}
