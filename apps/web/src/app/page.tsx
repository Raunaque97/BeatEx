import Link from "next/link";

import { SiteHeader } from "@/components/layout/site-header";
import { publicEnv } from "@/lib/config/public-env";

const layers = [
  ["Wallet + RPC", "Wallet Standard discovery, Solana Kit, devnet by default"],
  [
    "BeatEx SDK",
    "Typed transactions, accounts, private state, and order lifecycle",
  ],
  [
    "WASM prover",
    "Lazy proof assets and proof generation in a dedicated Web Worker",
  ],
  [
    "Feature UI",
    "Markets, auctions, orders, balances, portfolio, and activity",
  ],
] as const;

export default function HomePage() {
  return (
    <div className="app-frame">
      <SiteHeader />
      <main className="landing">
        <section className="hero">
          <p className="eyebrow">
            Confidential batch auctions · {publicEnv.cluster}
          </p>
          <h1>The frontend foundation is ready.</h1>
          <p className="hero-copy">
            The application shell is intentionally thin. Trading behavior will
            live in feature modules, while the existing SDK and browser prover
            remain the source of protocol logic.
          </p>
          <div className="hero-actions">
            <Link className="primary-action" href="/trade/tbtc-tusd">
              Open trading workspace
            </Link>
            <span className="network-pill">
              Program {shorten(publicEnv.programId)}
            </span>
          </div>
        </section>

        <section aria-label="Frontend layers" className="layer-grid">
          {layers.map(([title, detail], index) => (
            <article className="layer-card" key={title}>
              <span>{String(index + 1).padStart(2, "0")}</span>
              <h2>{title}</h2>
              <p>{detail}</p>
            </article>
          ))}
        </section>
      </main>
    </div>
  );
}

function shorten(value: string) {
  return `${value.slice(0, 4)}…${value.slice(-4)}`;
}
