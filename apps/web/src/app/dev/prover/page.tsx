import { notFound } from "next/navigation";

import { ProverBench } from "./prover-bench";

/** Development-only page for measuring browser proving latency. */
export default function ProverBenchPage() {
  if (process.env.NODE_ENV === "production") notFound();
  return <ProverBench />;
}
