import type { Metadata } from "next";

import { HomePage } from "@/features/home/components/home-page";

export const metadata: Metadata = {
  title: { absolute: "BeatEx · Private batch auctions on Solana" },
};

export default function Page() {
  return <HomePage />;
}
