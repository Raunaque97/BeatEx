import type { Metadata } from "next";
import { notFound } from "next/navigation";

import { TradingWorkspace } from "@/features/trading/components/trading-workspace";
import { findMarket } from "@/lib/config/markets";

type TradePageProps = {
  readonly params: Promise<{ market: string }>;
};

export async function generateMetadata({
  params,
}: TradePageProps): Promise<Metadata> {
  const market = findMarket((await params).market);
  return { title: market?.label ?? "Trade" };
}

export default async function TradePage({ params }: TradePageProps) {
  const market = findMarket((await params).market);
  if (market === undefined) notFound();

  return <TradingWorkspace market={market} />;
}
