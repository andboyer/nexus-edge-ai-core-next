// Admin Re-Identification page.
//
// Phase 5.6 · R7 — local-only diagnostic for the cross-camera re-ID
// pipeline. Shows the boot-time `[reid]` config snapshot + a live
// per-camera table with the worker's emit counters and the first
// 8 bytes (16 hex chars) of the most recent embedding. Refreshes
// every 5 s — short enough that walking in front of a camera and
// counting "ok, I see the counter bump" is one continuous gesture.
//
// When `reid.enabled = false`, the page falls back to a clear
// empty-state Card pointing at the toml + restart-required gotcha.

import { useQuery } from "@tanstack/react-query";
import { Fingerprint, Info } from "lucide-react";

import { getReidStatus } from "@/api/admin";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { formatAgo } from "@/lib/format";

export function AdminReidPage() {
  const statusQuery = useQuery({
    queryKey: ["admin", "reid", "status"],
    queryFn: () => getReidStatus(),
    // 5 s tracks the engine's per-track emit cadence well: each
    // refresh covers at most one new emit per active track, so the
    // counter delta the operator sees is meaningful.
    refetchInterval: 5_000,
  });

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-2xl font-semibold">Re-identification</h1>
        <p className="text-sm text-muted-foreground">
          Local diagnostic for the cross-camera re-ID worker. Per-camera
          counters refresh every 5 seconds — walk in front of a camera
          and watch the corresponding row tick.
        </p>
      </header>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <Fingerprint className="h-4 w-4 text-muted-foreground" />
            Pipeline status
          </CardTitle>
        </CardHeader>
        <CardContent>
          {statusQuery.isLoading ? (
            <Skeleton className="h-24 w-full" />
          ) : statusQuery.data ? (
            <ReidStatusBody data={statusQuery.data} />
          ) : (
            <p className="text-sm text-muted-foreground">
              Unable to load re-ID status.
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function ReidStatusBody({
  data,
}: {
  data: import("@/api/admin").ReidStatusResponse;
}) {
  if (!data.enabled) {
    return (
      <div className="rounded-md border border-dashed border-border bg-muted/20 p-4 text-sm">
        <div className="mb-2 flex items-center gap-2 text-sm font-medium">
          <Info className="h-4 w-4 text-muted-foreground" />
          Re-ID is disabled
        </div>
        <p className="text-muted-foreground">
          Set <code className="font-mono">[reid] enabled = true</code> in{" "}
          <code className="font-mono">/etc/nexus/nexus.toml</code> and
          restart the engine to start producing entity sightings. The model
          configured at startup is{" "}
          <code className="font-mono">{data.model_id}</code> (dim{" "}
          {data.dim}). Restart-required: this page picks up the new
          config on the next process start.
        </p>
      </div>
    );
  }
  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <ConfigTile label="Model" value={data.model_id} mono />
        <ConfigTile label="Embedding dim" value={String(data.dim)} />
        <ConfigTile
          label="Emit interval"
          value={`${data.emit_interval_s}s per track`}
        />
        <ConfigTile
          label="Min track age"
          value={`${data.min_track_age_frames} frames`}
        />
      </div>
      <CamerasTable cameras={data.cameras} />
    </div>
  );
}

function ConfigTile({
  label,
  value,
  mono,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="rounded-md border border-border bg-muted/20 p-3">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div
        className={
          "mt-1 text-sm font-semibold " + (mono ? "font-mono" : "")
        }
      >
        {value}
      </div>
    </div>
  );
}

function CamerasTable({
  cameras,
}: {
  cameras: import("@/api/admin").ReidCameraStatusRow[];
}) {
  if (cameras.length === 0) {
    return (
      <p className="rounded-md border border-dashed border-border bg-muted/10 px-4 py-6 text-center text-sm text-muted-foreground">
        No emits since boot. Walk in front of a camera and refresh — the
        first emit lands once a track has aged past{" "}
        <code className="font-mono">min_track_age_frames</code>.
      </p>
    );
  }
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs">
        <thead className="text-muted-foreground">
          <tr>
            <th className="px-2 py-1 text-left">Camera</th>
            <th className="px-2 py-1 text-right">Emits</th>
            <th className="px-2 py-1 text-left">Last emit</th>
            <th className="px-2 py-1 text-left">Last embedding (8 B)</th>
          </tr>
        </thead>
        <tbody>
          {cameras.map((c) => (
            <tr key={c.camera_id} className="border-t border-border/30">
              <td className="px-2 py-1">
                <Badge variant="secondary" className="font-mono">
                  cam {c.camera_id}
                </Badge>
              </td>
              <td className="px-2 py-1 text-right font-mono">
                {c.emit_count.toLocaleString()}
              </td>
              <td className="px-2 py-1 text-muted-foreground">
                {formatAgo(c.last_emit_at)}
              </td>
              <td className="px-2 py-1">
                <code className="font-mono">
                  {c.last_embedding_hex8 || "—"}
                </code>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
