// Top bar: brand + system-health pill + user menu + sign-out button.
// System-health pill polls /api/health every 10s.

import { useMutation, useQuery } from "@tanstack/react-query";
import { LogOut, ShieldCheck, User } from "lucide-react";
import { useState } from "react";

import { api } from "@/api/client";
import { authApi } from "@/api/auth";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useAuth, useSession } from "@/lib/auth";
import { cn } from "@/lib/utils";

interface HealthResponse {
  status: string;
  version?: string;
}

export function TopBar() {
  const session = useSession();
  const { clearSession } = useAuth();
  const [menuOpen, setMenuOpen] = useState(false);

  const health = useQuery({
    queryKey: ["health"],
    queryFn: () => api.get<HealthResponse>("/health"),
    refetchInterval: 10_000,
    retry: 1,
  });

  const logout = useMutation({
    mutationFn: () => authApi.logout(session?.refresh_token).catch(() => undefined),
    onSettled: () => {
      clearSession();
      // Hard reload to ensure no in-flight queries hold stale auth state.
      window.location.assign("/");
    },
  });

  const isHealthy = health.data?.status === "ok" || health.data?.status === "healthy";
  const healthVariant = health.isError
    ? "destructive"
    : isHealthy
      ? "success"
      : "warning";
  const healthLabel = health.isError
    ? "engine unreachable"
    : isHealthy
      ? `online \u2022 ${health.data?.version ?? "?"}`
      : "starting\u2026";

  return (
    <header className="flex h-12 shrink-0 items-center justify-between border-b border-border bg-card px-4">
      <div className="flex items-center gap-3">
        <div className="flex h-7 w-7 items-center justify-center rounded bg-primary/20 text-primary">
          <ShieldCheck className="h-4 w-4" />
        </div>
        <div className="text-sm font-semibold tracking-tight">Nexus Edge AI</div>
        <Badge variant={healthVariant} className="ml-2">
          {healthLabel}
        </Badge>
      </div>

      <div className="relative flex items-center gap-2">
        <Button
          variant="ghost"
          size="sm"
          className="gap-2"
          onClick={() => setMenuOpen((v) => !v)}
        >
          <User className="h-4 w-4" />
          <span className="text-sm">{session?.user.username ?? "guest"}</span>
          <span
            className={cn(
              "rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase",
              session?.user.role === "admin"
                ? "bg-primary/20 text-primary"
                : "bg-secondary text-muted-foreground",
            )}
          >
            {session?.user.role ?? "?"}
          </span>
        </Button>

        {menuOpen ? (
          <div
            className="absolute right-0 top-full z-50 mt-1 w-48 rounded-md border border-border bg-popover p-1 shadow-md"
            onMouseLeave={() => setMenuOpen(false)}
          >
            <button
              className="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm hover:bg-secondary"
              onClick={() => logout.mutate()}
              disabled={logout.isPending}
            >
              <LogOut className="h-4 w-4" />
              Sign out
            </button>
          </div>
        ) : null}
      </div>
    </header>
  );
}
