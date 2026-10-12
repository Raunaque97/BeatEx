import { redirect } from "next/navigation";

import { DEFAULT_MARKET_SLUG } from "@/lib/config/markets";

export default function HomePage() {
  redirect(`/trade/${DEFAULT_MARKET_SLUG}`);
}
