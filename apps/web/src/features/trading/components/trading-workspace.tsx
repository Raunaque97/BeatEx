"use client";

import { normalizeOraclePrice } from "@beatex/sdk";
import { useState } from "react";

import { BatchClock } from "@/features/auctions/components/batch-clock";
import { PriceChart } from "@/features/auctions/components/price-chart";
import { useMarketSnapshot } from "@/features/auctions/hooks/use-market-snapshot";
import { useOlderAuctions } from "@/features/auctions/hooks/use-older-auctions";
import { BalanceChip } from "@/features/balances/components/balance-chip";
import { FaucetBanner } from "@/features/balances/components/faucet-banner";
import {
  FundsDrawer,
  type FundsTab,
} from "@/features/balances/components/funds-drawer";
import { usePrivateState } from "@/features/balances/hooks/use-private-state";
import { MarketSelector } from "@/features/markets/components/market-selector";
import { OrderPanel } from "@/features/orders/components/order-panel";
import { SwapCard } from "@/features/orders/components/swap-card";
import { useOrderIntent } from "@/features/orders/hooks/use-order-intent";
import { WalletButton } from "@/features/wallet/components/wallet-button";
import type { MarketConfig } from "@/lib/config/markets";
import { formatPrice } from "@/lib/format";
import { portfolioValue } from "@/lib/funds";
import { useMarketFunds } from "@/features/balances/hooks/use-market-funds";
import { useBeatEx } from "@/providers/beatex-provider";

import { useAutoSettle } from "../hooks/use-auto-settle";
import { useTradeView, type TradeView } from "../hooks/use-trade-view";
import { MainAction } from "./main-action";

const VIEWS: readonly { readonly value: TradeView; readonly label: string }[] =
  [
    { value: "simple", label: "Simple" },
    { value: "advanced", label: "Advanced" },
  ];

export function TradingWorkspace({
  market,
}: {
  readonly market: MarketConfig;
}) {
  const [view, setView] = useTradeView();
  const [balances, setBalances] = useState<FundsTab | "overview" | null>(null);
  const { owner, client } = useBeatEx();
  const privateState = usePrivateState(market.address);
  const snapshot = useMarketSnapshot(market.address);
  // Lives here, not in the order button, so switching views never interrupts it.
  const settlement = useAutoSettle(market, privateState);
  const olderAuctions = useOlderAuctions(
    market.address,
    snapshot.market,
    snapshot.history.reduce<bigint | null>(
      (oldest, auction) =>
        oldest === null || auction.auctionId < oldest
          ? auction.auctionId
          : oldest,
      null,
    ),
  );
  const order = useOrderIntent(
    snapshot.batch?.grid ?? null,
    snapshot.market?.maxOrderQuantity,
  );

  // A never-deposited trader has a BeatEx balance of zero, not an unknown one.
  const beatexBalance =
    privateState.status === "ready"
      ? {
          baseAtoms: privateState.record.state.balance.baseValue,
          quoteAtoms: privateState.record.state.balance.quoteValue,
        }
      : privateState.status === "none"
        ? { baseAtoms: 0n, quoteAtoms: 0n }
        : null;

  const lastClear = snapshot.market?.lastClearingPrice ?? 0n;
  const headlinePrice =
    lastClear > 0n
      ? { label: "Last clear", price: lastClear }
      : snapshot.oracle !== null && snapshot.market !== undefined
        ? {
            label: "Pyth",
            price: normalizeOraclePrice(
              snapshot.oracle.price,
              snapshot.oracle.exponent,
              snapshot.market.oracleExponentAdjustment,
            ),
          }
        : null;

  // One balance per token across every market; an order moves funds here
  // from the others when it needs them.
  const funds = useMarketFunds(
    market,
    privateState,
    headlinePrice?.price ?? null,
  );
  const mints =
    snapshot.market === undefined
      ? null
      : { base: snapshot.market.baseMint, quote: snapshot.market.quoteMint };
  const spendable =
    beatexBalance === null || mints === null
      ? beatexBalance
      : {
          baseAtoms:
            funds.tokens.get(mints.base)?.free ?? beatexBalance.baseAtoms,
          quoteAtoms:
            funds.tokens.get(mints.quote)?.free ?? beatexBalance.quoteAtoms,
        };
  const portfolioTotal = funds.complete ? portfolioValue(funds.markets) : null;
  const portfolioEmpty =
    funds.complete &&
    [...funds.tokens.values()].every(
      (token) => token.free === 0n && token.locked === 0n,
    );

  const showAdvanced = () => setView("advanced");
  const { intent, built } = order;
  const orderLabel =
    intent.type === "custom"
      ? "Seal custom order"
      : intent.type === "limit"
        ? `Place limit ${intent.side}`
        : `${intent.side === "buy" ? "Buy" : "Sell"} ${market.base.symbol}`;
  const orderError =
    built !== null &&
    !built.ok &&
    (intent.type === "custom" || intent.amount !== "")
      ? built.error
      : null;
  const action = (
    <MainAction
      onDeposit={() => setBalances("deposit")}
      settlement={settlement}
      funds={funds.tokens}
      mints={mints}
      market={market}
      privateState={privateState}
      orderLabel={orderLabel}
      orderError={orderError}
      order={built?.ok ? built.order : null}
      batch={snapshot.batch}
    />
  );
  const limitPrice =
    order.intent.type === "limit"
      ? (snapshot.batch?.grid?.[order.intent.limitIndex]?.price ?? null)
      : null;

  return (
    <div
      className={`flex min-h-dvh flex-col ${
        // Advanced fits the window: the order panel scrolls inside itself, so
        // progress lines appearing under the button never grow the page.
        view === "advanced" ? "lg:h-dvh lg:overflow-hidden" : ""
      }`}
    >
      <FaucetBanner
        key={owner ?? "none"}
        market={market}
        privateState={privateState}
      />
      <header className="border-line flex flex-wrap items-center justify-between gap-4 border-b px-5 py-2.5">
        <div className="flex flex-wrap items-center gap-7">
          <div className="leading-tight">
            <div className="text-lg font-bold tracking-tight">
              Beat<span className="text-accent">Ex</span>
            </div>
            <div className="text-muted text-xs">Frequent batch auction</div>
          </div>
          <MarketSelector market={market} />
          <div>
            <div className="text-muted text-xs">
              {headlinePrice?.label ?? "Price"}
            </div>
            <div className="font-mono text-lg">
              {headlinePrice === null ? "—" : formatPrice(headlinePrice.price)}
            </div>
          </div>
          <BatchClock
            schedule={snapshot.market}
            timing={snapshot.slotTiming}
            phase={snapshot.batch?.phase ?? null}
          />
        </div>
        <div className="flex flex-wrap items-center gap-3">
          <div
            role="group"
            aria-label="Interface"
            className="border-line bg-panel flex rounded-lg border p-[3px]"
          >
            {VIEWS.map(({ value: option, label }) => (
              <button
                key={option}
                type="button"
                aria-pressed={view === option}
                onClick={() => setView(option)}
                className={`min-h-9 rounded-md px-3.5 text-sm ${
                  view === option
                    ? "bg-accent text-accent-ink font-semibold"
                    : "text-muted hover:text-text"
                }`}
              >
                {label}
              </button>
            ))}
          </div>
          {owner !== null && client !== null ? (
            <BalanceChip
              value={portfolioTotal}
              empty={portfolioEmpty}
              onOpen={() => setBalances("overview")}
            />
          ) : null}
          <WalletButton />
        </div>
      </header>

      {snapshot.marketError ? (
        <div
          role="alert"
          className="text-warn mx-auto mt-10 max-w-md px-5 text-center text-sm"
        >
          Could not load {market.label} from {"the configured cluster"}.{" "}
          {snapshot.marketError.message}
        </div>
      ) : view === "simple" ? (
        <main className="flex flex-col items-center px-5 pt-14 pb-20">
          <SwapCard
            market={market}
            marketAccount={snapshot.market}
            batch={snapshot.batch}
            order={order}
            action={action}
            balance={spendable}
            onAdvanced={showAdvanced}
          />
        </main>
      ) : (
        <main className="flex flex-1 flex-col lg:min-h-0 lg:flex-row">
          <section
            aria-label="Price action"
            className="border-line flex h-[460px] min-w-0 flex-col border-b px-4 py-3 lg:h-auto lg:min-h-0 lg:flex-1 lg:border-r lg:border-b-0"
          >
            <div className="text-muted mb-1.5 flex flex-wrap justify-between gap-2 text-xs">
              <span>Price per auction · on-chain</span>
            </div>
            <div className="min-h-0 flex-1">
              <PriceChart
                history={snapshot.history}
                older={olderAuctions.batches}
                onNeedOlder={olderAuctions.loadOlder}
                batch={snapshot.batch}
                lastClearingPrice={lastClear > 0n ? lastClear : null}
                limitPrice={limitPrice}
              />
            </div>
          </section>
          <div className="w-full flex-none px-5 py-3 lg:w-[460px] lg:overflow-y-auto">
            <OrderPanel
              market={market}
              marketAccount={snapshot.market}
              batch={snapshot.batch}
              order={order}
              action={action}
              balance={spendable}
            />
          </div>
        </main>
      )}

      {balances !== null && owner !== null && client !== null ? (
        <FundsDrawer
          market={market}
          privateState={privateState}
          funds={funds}
          mints={mints}
          initialTab={balances === "overview" ? undefined : balances}
          schedule={snapshot.market}
          onClose={() => setBalances(null)}
        />
      ) : null}
    </div>
  );
}
