"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";

import { MARKETS, type MarketConfig } from "@/lib/config/markets";

export function MarketSelector({ market }: { readonly market: MarketConfig }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent | KeyboardEvent) => {
      if (
        event instanceof KeyboardEvent
          ? event.key === "Escape"
          : !root.current?.contains(event.target as Node)
      ) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open]);

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
        className="flex min-h-11 items-center gap-2.5 text-left"
      >
        <span
          aria-hidden="true"
          className="flex h-8 w-8 items-center justify-center rounded-full bg-[#f2a33a] text-sm font-bold text-[#2a1600]"
        >
          ₿
        </span>
        <span className="text-[17px] font-semibold">
          {market.label} <span className="text-muted text-sm">▾</span>
        </span>
      </button>
      {open ? (
        <ul
          role="listbox"
          aria-label="Markets"
          className="border-line-strong bg-panel absolute top-14 left-0 z-20 w-64 rounded-xl border p-1.5 shadow-2xl shadow-black/50"
        >
          {MARKETS.map((option) => (
            <li
              key={option.slug}
              role="option"
              aria-selected={option.slug === market.slug}
            >
              <Link
                href={`/trade/${option.slug}`}
                onClick={() => setOpen(false)}
                className={`flex rounded-lg px-3 py-2.5 ${option.slug === market.slug ? "bg-raised" : "hover:bg-raised"}`}
              >
                {option.label}
              </Link>
            </li>
          ))}
          <li className="text-faint px-3 py-2 text-xs">
            More markets as they launch on-chain
          </li>
        </ul>
      ) : null}
    </div>
  );
}
