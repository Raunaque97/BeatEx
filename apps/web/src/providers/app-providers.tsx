"use client";

import { QueryClientProvider } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";

import { createQueryClient } from "@/lib/query/create-query-client";
import { BeatExProvider } from "@/providers/beatex-provider";

export function AppProviders({ children }: Readonly<{ children: ReactNode }>) {
  const [queryClient] = useState(createQueryClient);

  return (
    <QueryClientProvider client={queryClient}>
      <BeatExProvider>{children}</BeatExProvider>
    </QueryClientProvider>
  );
}
