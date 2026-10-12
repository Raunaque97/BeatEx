import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useTradeView } from "./use-trade-view";

afterEach(() => window.localStorage.clear());

describe("useTradeView", () => {
  it("starts simple and remembers a switch for the next page", () => {
    const first = renderHook(() => useTradeView());
    expect(first.result.current[0]).toBe("simple");
    act(() => first.result.current[1]("advanced"));
    expect(first.result.current[0]).toBe("advanced");
    first.unmount();

    // A new market page mounts a fresh workspace; the choice carries over.
    const next = renderHook(() => useTradeView());
    expect(next.result.current[0]).toBe("advanced");
  });
});
