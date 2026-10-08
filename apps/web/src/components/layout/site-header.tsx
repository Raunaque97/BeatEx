import Link from "next/link";

import { publicEnv } from "@/lib/config/public-env";

export function SiteHeader() {
  return (
    <header className="site-header">
      <Link className="brand" href="/">
        Beat<span className="brand-mark">Ex</span>
      </Link>
      <nav aria-label="Primary navigation" className="site-nav">
        <Link href="/trade/tbtc-tusd">Trade</Link>
        <span>
          <span aria-hidden="true" className="status-dot" />
          {publicEnv.cluster}
        </span>
      </nav>
    </header>
  );
}
