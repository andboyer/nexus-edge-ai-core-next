import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { Toaster } from "sonner";

import { AuthProvider } from "@/lib/auth";
import { router } from "@/router";

import "./index.css";

// One QueryClient per app lifetime. Defaults err on the side of
// fresh data because operators expect the console to mirror the
// engine state immediately when they hit Save.
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 5_000,
      gcTime: 5 * 60_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
    mutations: {
      retry: 0,
    },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <AuthProvider>
        <RouterProvider router={router} />
        <Toaster
          theme="dark"
          position="bottom-right"
          richColors
          closeButton
        />
      </AuthProvider>
    </QueryClientProvider>
  </StrictMode>,
);

