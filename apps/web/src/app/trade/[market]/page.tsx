import type { Metadata } from "next";

import { TradingWorkspace } from "@/features/trading/components/trading-workspace";

export const metadata: Metadata = {
  title: "Trade",
};

type TradePageProps = {
  readonly params: Promise<{ market: string }>;
};

export default async function TradePage({ params }: TradePageProps) {
  const { market } = await params;

  return <TradingWorkspace marketSlug={market} />;
}
