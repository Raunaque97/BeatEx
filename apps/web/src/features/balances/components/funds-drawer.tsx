"use client";

import {
  formatDecimalUnits,
  parseDecimalUnits,
  type MarketAccount,
} from "@beatex/sdk";
import { useEffect, useRef, useState } from "react";

import { PercentSlider, percentOf, shareOf } from "@/components/percent-slider";
import { TxProgressLine } from "@/components/tx-progress-line";
import type { MarketConfig } from "@/lib/config/markets";
import { sanitizeDecimal } from "@/lib/decimal-input";
import { formatQuote, groupDecimal } from "@/lib/format";
import {
  planMoves,
  portfolioValue,
  type MarketFunds,
  type TokenFunds,
} from "@/lib/funds";
import { useBeatEx, useTrackedAction } from "@/providers/beatex-provider";

import { useFaucet } from "../hooks/use-faucet";
import type { PrivateStateView } from "../hooks/use-private-state";
import {
  useWalletBalances,
  useWalletTokens,
  type WalletMints,
} from "../hooks/use-wallet-balances";
import { HistoryList } from "./history-list";
import { TokenSelect } from "./token-select";

export type FundsTab = "deposit" | "withdraw";
type Token = "base" | "quote";

/** Parse a typed amount into atoms, or null when it is not a number yet. */
function parseAtoms(text: string, decimals: number) {
  if (text === "" || text === ".") return 0n;
  try {
    return parseDecimalUnits(text, decimals);
  } catch {
    return null;
  }
}

/** Token atoms as a grouped decimal amount, for any token's decimals. */
function formatAtoms(atoms: bigint, decimals: number): string {
  return groupDecimal(formatDecimalUnits(atoms, decimals));
}

/** A table amount: quote tokens to cents (like tUSD), others in full. */
function formatToken(atoms: bigint, token: TokenFunds): string {
  return token.quote && token.decimals >= 2
    ? groupDecimal(
        formatDecimalUnits(atoms / 10n ** BigInt(token.decimals - 2), 2),
      )
    : formatAtoms(atoms, token.decimals);
}

/**
 * Balances, opened from the header chip or a deposit button: the portfolio
 * and one balance per token across every market, then one token at a time to
 * deposit into or withdraw from BeatEx. Which market holds what never shows:
 * withdrawals draw on other markets automatically.
 */
export function FundsDrawer({
  market,
  privateState,
  funds,
  mints,
  initialTab,
  schedule,
  onClose,
}: {
  readonly market: MarketConfig;
  readonly privateState: PrivateStateView;
  /** The trader's funds in every market. */
  readonly funds: {
    readonly markets: readonly MarketFunds[];
    readonly tokens: ReadonlyMap<string, TokenFunds>;
    readonly complete: boolean;
  };
  /** This market's token mints, once its account has loaded. */
  readonly mints: WalletMints | null;
  readonly initialTab?: FundsTab;
  /** The market schedule, so history can show each order's batch. */
  readonly schedule?: MarketAccount;
  readonly onClose: () => void;
}) {
  const { client, owner } = useBeatEx();
  const wallet = useWalletBalances(owner, mints);
  const walletTokens = useWalletTokens(owner, [...funds.tokens.keys()]);
  const faucet = useFaucet();
  const action = useTrackedAction();
  const record = privateState.status === "ready" ? privateState.record : null;
  const walletBase = wallet.data?.baseAtoms ?? 0n;
  const walletQuote = wallet.data?.quoteAtoms ?? 0n;
  // Free BeatEx funds of this market's tokens, counting every market.
  const freeHere = {
    base: record?.state.balance.baseValue ?? 0n,
    quote: record?.state.balance.quoteValue ?? 0n,
  };
  const freeAnywhere = {
    base:
      (mints === null ? undefined : funds.tokens.get(mints.base)?.free) ??
      freeHere.base,
    quote:
      (mints === null ? undefined : funds.tokens.get(mints.quote)?.free) ??
      freeHere.quote,
  };
  const [tab, setTab] = useState<FundsTab>(
    initialTab ??
      (walletBase > 0n ||
      walletQuote > 0n ||
      (freeAnywhere.base === 0n && freeAnywhere.quote === 0n)
        ? "deposit"
        : "withdraw"),
  );
  const [picked, setPicked] = useState<Token | null>(null);
  // null follows the full available balance (Max), even as it loads.
  const [text, setText] = useState<string | null>(null);
  const [view, setView] = useState<"balances" | "history">("balances");
  const panel = useRef<HTMLElement>(null);

  useEffect(() => {
    panel.current?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const pending = "…";
  // One row per token across every market; this market's tokens first.
  const order = [mints?.base, mints?.quote];
  const tokenRows = [...funds.tokens.values()].sort(
    (left, right) =>
      ((order.indexOf(left.mint) + 3) % 3) -
      ((order.indexOf(right.mint) + 3) % 3),
  );
  const rows = [
    ...tokenRows.map((token) => ({
      symbol: token.symbol,
      note: null as string | null,
      beatex: funds.complete
        ? formatToken(token.free + token.locked, token)
        : pending,
      locked:
        token.locked > 0n
          ? `${formatToken(token.locked, token)} in order`
          : null,
      wallet:
        walletTokens.data === undefined
          ? pending
          : formatAtoms(
              walletTokens.data.get(token.mint) ?? 0n,
              token.decimals,
            ),
    })),
    {
      symbol: "SOL",
      note: "Network fees" as string | null,
      beatex: "—",
      locked: null,
      wallet:
        wallet.data === undefined
          ? pending
          : formatDecimalUnits(wallet.data.lamports, 9).slice(0, 6),
    },
  ];
  const total = funds.complete ? portfolioValue(funds.markets) : null;

  // Deposits draw on the wallet; withdrawals on free BeatEx funds in any
  // market (funds locked in an open order stay put until it settles).
  const available = {
    base: tab === "deposit" ? walletBase : freeAnywhere.base,
    quote: tab === "deposit" ? walletQuote : freeAnywhere.quote,
  };
  const tokens = {
    base: { symbol: market.base.symbol, decimals: market.base.decimals },
    quote: { symbol: market.quote.symbol, decimals: market.quote.decimals },
  } as const;
  const token: Token =
    picked ??
    (available.quote > 0n ? "quote" : available.base > 0n ? "base" : "quote");
  const info = tokens[token];
  const max = available[token];
  const atoms = text === null ? max : parseAtoms(text, info.decimals);
  const amountError =
    atoms === null
      ? "Enter a valid amount."
      : atoms > max
        ? tab === "deposit"
          ? "More than your wallet holds."
          : "More than your free BeatEx balance."
        : null;
  const amount = atoms ?? 0n;
  const percent = percentOf(amount, max);
  const nothingAvailable = available.base === 0n && available.quote === 0n;
  const canSubmit =
    client !== null &&
    wallet.data !== undefined &&
    amountError === null &&
    amount > 0n &&
    !action.busy &&
    privateState.status !== "pending" &&
    (tab === "deposit" || privateState.status === "ready");

  const choose = (next: { tab?: FundsTab; token?: Token }) => {
    if (next.tab !== undefined) setTab(next.tab);
    if (next.token !== undefined) setPicked(next.token);
    setText(null);
    action.reset();
  };
  const slide = (value: number) =>
    setText(
      value >= 100
        ? null
        : formatDecimalUnits(shareOf(max, value), info.decimals),
    );

  const submit = async () => {
    if (!canSubmit || client === null || wallet.data === undefined) return;
    const data = wallet.data;
    const baseAmount = token === "base" ? amount : 0n;
    const quoteAmount = token === "quote" ? amount : 0n;
    try {
      if (tab === "withdraw" && mints !== null && amount > freeHere[token]) {
        // Gather the token here from the other markets first.
        const moves =
          planMoves(funds.tokens, market.address, mints[token], amount) ?? [];
        for (const move of moves) {
          await action.run((proofOptions) =>
            client.moveBetweenMarkets({
              from: move.from.address,
              to: market.address,
              mint: mints[token],
              amount: move.amount,
              proofOptions,
            }),
          );
        }
      }
      await action.run<unknown>((proofOptions) =>
        tab === "deposit"
          ? client.deposit({
              market: market.address,
              baseSource: data.baseAccount,
              quoteSource: data.quoteAccount,
              baseAmount,
              quoteAmount,
              proofOptions,
            })
          : client.withdraw({
              market: market.address,
              baseDestination: data.baseAccount,
              quoteDestination: data.quoteAccount,
              baseAmount,
              quoteAmount,
              proofOptions,
            }),
      );
      setText(null);
    } catch {
      // Shown by the progress line.
    }
  };

  return (
    <>
      <div
        aria-hidden="true"
        onClick={onClose}
        className="fixed inset-0 z-40 bg-black/50"
      />
      <aside
        ref={panel}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-label="Balances"
        className="border-line-strong bg-panel fixed top-0 right-0 bottom-0 z-50 flex w-[380px] max-w-full flex-col gap-5 overflow-y-auto border-l p-5 outline-none"
      >
        <div className="flex items-center justify-between">
          <div role="tablist" aria-label="Wallet panel" className="flex gap-1">
            {(["balances", "history"] as const).map((option) => (
              <button
                key={option}
                type="button"
                role="tab"
                aria-selected={view === option}
                onClick={() => setView(option)}
                className={`min-h-9 rounded-md px-2.5 text-base ${
                  view === option
                    ? "text-text font-semibold"
                    : "text-muted hover:text-text"
                }`}
              >
                {option === "balances" ? "Balances" : "History"}
              </button>
            ))}
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="text-muted hover:text-text h-9 w-9 text-xl"
          >
            ×
          </button>
        </div>

        {view === "history" ? (
          owner === null ? null : (
            <HistoryList owner={owner} schedule={schedule} />
          )
        ) : (
          <>
            {privateState.status === "pending" ? (
              <p className="text-warn text-xs leading-relaxed">
                Finishing your last transaction. Deposits and withdrawals resume
                once it&apos;s confirmed.
              </p>
            ) : null}

            <section aria-label="Portfolio" className="flex flex-col gap-1">
              <div className="text-muted text-xs">Portfolio</div>
              <div className="font-mono text-3xl">
                {total === null ? "…" : `$${formatQuote(total)}`}
              </div>
              <p className="text-muted text-xs leading-relaxed">
                Everything deposited in BeatEx across every market, including
                funds in an open order. Encrypted on Solana, so only you can see
                it.
              </p>
            </section>

            <table className="w-full text-sm">
              <thead>
                <tr className="text-muted border-line border-b text-xs">
                  <th className="pb-2 text-left font-normal">Token</th>
                  <th className="pb-2 text-right font-normal">BeatEx</th>
                  <th className="pb-2 text-right font-normal">Wallet</th>
                </tr>
              </thead>
              <tbody className="font-mono">
                {rows.map((row) => (
                  <tr
                    key={row.symbol}
                    className="border-line border-b last:border-0"
                  >
                    <td className="py-2.5 font-sans">
                      {row.symbol}
                      {row.note === null ? null : (
                        <div className="text-faint text-xs">{row.note}</div>
                      )}
                    </td>
                    <td className="py-2.5 text-right">
                      {row.beatex}
                      {row.locked === null ? null : (
                        <div className="text-faint text-xs">{row.locked}</div>
                      )}
                    </td>
                    <td className="py-2.5 text-right">{row.wallet}</td>
                  </tr>
                ))}
              </tbody>
            </table>

            <section
              aria-label="Move funds"
              className="border-line flex flex-col gap-4 rounded-xl border p-3.5"
            >
              <div
                role="tablist"
                aria-label="Funds action"
                className="border-line grid grid-cols-2 rounded-lg border p-[3px]"
              >
                {(["deposit", "withdraw"] as const).map((option) => (
                  <button
                    key={option}
                    type="button"
                    role="tab"
                    aria-selected={tab === option}
                    onClick={() => choose({ tab: option, token: undefined })}
                    className={`min-h-9 rounded-md text-sm ${tab === option ? "bg-raised font-semibold" : "text-muted"}`}
                  >
                    {option === "deposit"
                      ? "Deposit from wallet"
                      : "Withdraw to wallet"}
                  </button>
                ))}
              </div>

              {nothingAvailable ? (
                <div className="flex flex-col gap-2.5">
                  <p className="text-muted text-sm">
                    {tab === "deposit"
                      ? "Your wallet has no test tokens to deposit."
                      : "Nothing to withdraw yet."}
                  </p>
                  {tab === "deposit" ? (
                    <button
                      type="button"
                      disabled={faucet.status === "sending" || owner === null}
                      onClick={() => void faucet.request()}
                      className="border-line-strong min-h-11 rounded-xl border text-sm font-semibold disabled:opacity-50"
                    >
                      {faucet.status === "sending"
                        ? "Sending test funds…"
                        : "Get test funds"}
                    </button>
                  ) : null}
                  {faucet.message !== null ? (
                    <p className="text-muted text-xs">{faucet.message}</p>
                  ) : null}
                </div>
              ) : (
                <>
                  <div className="flex flex-col gap-1.5">
                    <div className="text-muted flex justify-between text-xs">
                      <label htmlFor="funds-amount">Amount</label>
                      <span>
                        Available{" "}
                        <span className="text-text font-mono">
                          {formatAtoms(max, info.decimals)}
                        </span>{" "}
                        {info.symbol}
                      </span>
                    </div>
                    <div className="bg-bg focus-within:border-accent/70 flex items-center gap-2 rounded-xl border border-transparent py-2 pr-2 pl-3.5 transition-colors">
                      <input
                        id="funds-amount"
                        aria-label={`${info.symbol} amount`}
                        type="text"
                        inputMode="decimal"
                        autoComplete="off"
                        placeholder="0"
                        value={text ?? formatDecimalUnits(max, info.decimals)}
                        onChange={(event) =>
                          setText(
                            sanitizeDecimal(event.target.value, info.decimals),
                          )
                        }
                        className="placeholder:text-faint h-9 min-w-0 flex-1 bg-transparent font-mono text-2xl outline-none focus-visible:outline-none"
                      />
                      <TokenSelect
                        label="Token"
                        value={token}
                        onChange={(next) => choose({ token: next })}
                        options={(["base", "quote"] as const).map((option) => ({
                          id: option,
                          symbol: tokens[option].symbol,
                          balance: formatAtoms(
                            available[option],
                            tokens[option].decimals,
                          ),
                        }))}
                      />
                    </div>
                  </div>

                  <PercentSlider
                    label="Share of available balance"
                    value={percent}
                    disabled={max === 0n}
                    onChange={slide}
                  />

                  {action.progress.phase !== "idle" ? (
                    <TxProgressLine
                      title={tab === "deposit" ? "Deposit" : "Withdrawal"}
                      progress={action.progress}
                    />
                  ) : null}

                  <button
                    type="button"
                    disabled={!canSubmit}
                    onClick={() => void submit()}
                    className="bg-accent text-accent-ink min-h-12 rounded-xl font-semibold disabled:cursor-not-allowed disabled:opacity-45"
                  >
                    {max === 0n
                      ? tab === "deposit"
                        ? `No ${info.symbol} in your wallet`
                        : `No free ${info.symbol} to withdraw`
                      : amount === 0n
                        ? "Enter an amount"
                        : `${tab === "deposit" ? "Deposit" : "Withdraw"} ${formatAtoms(amount, info.decimals)} ${info.symbol}`}
                  </button>
                  <p
                    className={`-mt-2 text-xs leading-relaxed ${amountError === null ? "text-muted" : "text-warn"}`}
                  >
                    {amountError ??
                      (tab === "deposit"
                        ? "Deposits are encrypted, so nobody can see what you trade."
                        : "Withdrawals are public: they show the amount leaving BeatEx.")}
                  </p>
                </>
              )}
            </section>
          </>
        )}
      </aside>
    </>
  );
}
