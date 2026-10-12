import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  poweredByHeader: false,
  reactStrictMode: true,
  typedRoutes: true,
  transpilePackages: ["@beatex/sdk", "@beatex/prover-wasm"],
  async rewrites() {
    const configured = process.env.BEATEX_INDEXER_URL;
    if (process.env.VERCEL && !configured) {
      throw new Error(
        "Set BEATEX_INDEXER_URL to the indexer's HTTPS tunnel origin before deploying.",
      );
    }
    const origin = new URL(configured ?? "http://127.0.0.1:8787");
    if (
      !["http:", "https:"].includes(origin.protocol) ||
      origin.username ||
      origin.password ||
      origin.search ||
      origin.hash ||
      origin.pathname !== "/" ||
      (process.env.VERCEL && origin.protocol !== "https:")
    ) {
      throw new Error(
        "BEATEX_INDEXER_URL must be an HTTP(S) origin without credentials, path, query, or fragment; Vercel requires HTTPS.",
      );
    }
    return [
      {
        source: "/api/indexer/v1/markets/:market/auctions",
        destination: `${origin.origin}/v1/markets/:market/auctions`,
      },
    ];
  },
  async headers() {
    return [
      {
        source: "/api/indexer/v1/markets/:market/auctions",
        // Also opt older Vercel projects into honoring upstream cache headers.
        headers: [{ key: "x-vercel-enable-rewrite-caching", value: "1" }],
      },
    ];
  },
};

export default nextConfig;
