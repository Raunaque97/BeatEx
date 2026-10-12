"use client";

import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";

export interface TokenOption<Id extends string> {
  readonly id: Id;
  readonly symbol: string;
  /** Pre-formatted balance shown beside the symbol. */
  readonly balance: string;
}

/**
 * A compact token picker: a button showing the chosen symbol that opens a list
 * of every token with its balance. Scales to any number of tokens.
 */
export function TokenSelect<Id extends string>({
  label,
  options,
  value,
  onChange,
}: {
  readonly label: string;
  readonly options: readonly TokenOption<Id>[];
  readonly value: Id;
  readonly onChange: (id: Id) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const listId = useId();
  const selected = options.find((option) => option.id === value);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const show = () => {
    setActive(
      Math.max(
        0,
        options.findIndex((option) => option.id === value),
      ),
    );
    setOpen(true);
  };
  const pick = (id: Id) => {
    onChange(id);
    setOpen(false);
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) {
        event.preventDefault();
        show();
      }
      return;
    }
    if (event.key === "Escape") {
      // Close only the list, not the panel around it.
      event.stopPropagation();
      setOpen(false);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive((index) => (index + step + options.length) % options.length);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const option = options[active];
      if (option !== undefined) pick(option.id);
    }
  };

  return (
    <div ref={root} className="relative shrink-0" onKeyDown={onKeyDown}>
      <button
        type="button"
        aria-label={`${label}: ${selected?.symbol ?? ""}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        onClick={() => (open ? setOpen(false) : show())}
        className="border-line-strong bg-raised flex min-h-9 items-center gap-1.5 rounded-full border px-3.5 text-sm font-semibold"
      >
        {selected?.symbol}
        <svg
          aria-hidden="true"
          width="10"
          height="10"
          viewBox="0 0 10 10"
          className={`text-muted transition-transform ${open ? "rotate-180" : ""}`}
        >
          <path
            d="M2 3.5l3 3 3-3"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
          />
        </svg>
      </button>
      {open ? (
        <ul
          id={listId}
          role="listbox"
          aria-label={label}
          className="border-line-strong bg-panel absolute top-11 right-0 z-10 min-w-48 rounded-xl border p-1 shadow-2xl shadow-black/50"
        >
          {options.map((option, index) => (
            <li
              key={option.id}
              role="option"
              aria-selected={option.id === value}
              aria-label={`${option.symbol} ${option.balance}`}
              onMouseEnter={() => setActive(index)}
              onClick={() => pick(option.id)}
              className={`flex cursor-pointer items-center justify-between gap-4 rounded-lg px-3 py-2 text-sm ${
                index === active ? "bg-raised" : ""
              }`}
            >
              <span
                className={
                  option.id === value ? "text-accent font-semibold" : ""
                }
              >
                {option.symbol}
              </span>
              <span className="text-muted font-mono text-xs">
                {option.balance}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
