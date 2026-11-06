/**
 * Ask for a counterparty in the next auction, and with `open` for the running
 * auction to be opened now. Best effort: an order can still go in without one.
 */
export async function requestMaker(
  market: string,
  owner: string,
  { open = false }: { readonly open?: boolean } = {},
): Promise<void> {
  try {
    await fetch("/api/maker", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ market, owner, open }),
    });
  } catch {
    // The order's own wait reports an auction that never opened.
  }
}
