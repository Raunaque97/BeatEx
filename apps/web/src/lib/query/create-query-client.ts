import { QueryClient } from "@tanstack/react-query";

export function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        refetchOnWindowFocus: false,
        // Polls retry on their next interval; back off rather than pile onto a
        // rate-limited RPC.
        retry: 1,
        retryDelay: 3_000,
        staleTime: 5_000,
      },
      mutations: {
        retry: false,
      },
    },
  });
}
