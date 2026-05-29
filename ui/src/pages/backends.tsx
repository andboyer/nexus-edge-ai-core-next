// /backends — inference backend slot status.

import { useQuery } from "@tanstack/react-query";
import { Server } from "lucide-react";

import { getBackends } from "@/api/system";
import type { BackendStatus } from "@/api/types";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { PageHeader } from "@/pages/placeholder";

export function BackendsPage() {
  const q = useQuery({
    queryKey: ["backends", "full"],
    queryFn: getBackends,
    refetchInterval: 5_000,
  });

  return (
    <div className="flex flex-col gap-6">
      <PageHeader
        title="Inference Backends"
        description="Detector worker pool slot status and active model kinds."
      />

      {q.isLoading ? (
        <Skeleton className="h-32 w-full" />
      ) : q.isError ? (
        <Card>
          <CardContent className="py-6 text-sm text-muted-foreground">
            Failed to load backends. {q.error instanceof Error ? q.error.message : ""}
          </CardContent>
        </Card>
      ) : q.data ? (
        <>
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="flex items-center gap-2 text-base">
                <Server className="h-4 w-4" />
                Mode
              </CardTitle>
            </CardHeader>
            <CardContent>
              <Badge variant="outline" className="uppercase">
                {q.data.mode}
              </Badge>
              <p className="mt-2 text-xs text-muted-foreground">
                {q.data.mode === "in_process"
                  ? "Detector runs inside the engine process. No worker pool slots are reported in this mode."
                  : "Detector is sharded across one or more worker processes."}
              </p>
            </CardContent>
          </Card>

          {q.data.slots.length > 0 && (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="text-base">Slots</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead className="text-left text-xs uppercase text-muted-foreground">
                      <tr>
                        <th className="px-2 py-2">Slot</th>
                        <th className="px-2 py-2">Name</th>
                        <th className="px-2 py-2">State</th>
                        <th className="px-2 py-2">Generation</th>
                      </tr>
                    </thead>
                    <tbody>
                      {q.data.slots.map((s) => (
                        <tr key={s.slot} className="border-t border-border/40">
                          <td className="px-2 py-2 font-mono">{s.slot}</td>
                          <td className="px-2 py-2">{s.name}</td>
                          <td className="px-2 py-2">
                            <StateBadge state={s.state} />
                          </td>
                          <td className="px-2 py-2 font-mono">{s.generation}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </CardContent>
            </Card>
          )}
        </>
      ) : null}
    </div>
  );
}

function StateBadge({ state }: { state: BackendStatus["state"] }) {
  // Engine emits one of: initializing | ready | restarting | failed
  // (see BackendState in crates/nexus-inference/src/backends.rs).
  const s = String(state).toLowerCase();
  const variant: "success" | "warning" | "destructive" | "secondary" =
    s === "ready"
      ? "success"
      : s === "initializing" || s === "restarting"
        ? "warning"
        : s === "failed"
          ? "destructive"
          : "secondary";
  return <Badge variant={variant}>{s}</Badge>;
}
