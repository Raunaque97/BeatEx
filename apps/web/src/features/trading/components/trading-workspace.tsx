import Link from "next/link";

type TradingWorkspaceProps = {
  readonly marketSlug: string;
};

export function TradingWorkspace({ marketSlug }: TradingWorkspaceProps) {
  const marketLabel = marketSlug.replace("-", " / ").toUpperCase();

  return (
    <main className="workspace">
      <header className="workspace-header">
        <div>
          <h1>{marketLabel}</h1>
          <p>Trading workspace structure</p>
        </div>
        <Link href="/">BeatEx</Link>
      </header>
      <section className="workspace-panel" aria-label="Market chart">
        <h2>Market + auction</h2>
        <p>
          Price chart, auction timer, clearing history, and aggregate curves.
        </p>
      </section>
      <aside className="workspace-panel" aria-label="Order entry">
        <h2>Private order</h2>
        <p>
          Wallet, private balances, encrypted curve editor, and proof progress.
        </p>
      </aside>
      <section
        className="workspace-orders workspace-panel"
        aria-label="Orders and balances"
      >
        <h2>Orders + portfolio</h2>
        <p>
          Active order, replace/cancel actions, claims, withdrawals, and
          activity.
        </p>
      </section>
    </main>
  );
}
