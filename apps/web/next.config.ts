import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  poweredByHeader: false,
  reactStrictMode: true,
  typedRoutes: true,
  transpilePackages: ["@beatex/sdk", "@beatex/prover-wasm"],
};

export default nextConfig;
