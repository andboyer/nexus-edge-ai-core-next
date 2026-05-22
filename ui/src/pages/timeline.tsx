// Per-camera motion timeline.
//
// Top: camera picker + bucket-size selector + date-range picker.
// Body: a wide horizontal histogram of motion events. Click a bar to
// expand the underlying MotionEventRow list for that bucket window.

import { useQuery } from "@tanstack/react-query";
import { BarChart3, Calendar, Play, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import {
  getCameraMotionHistogram,
  listCameraMotion,
  listCameras,
} from "@/api/system";
import type {
  MotionEventRow,
  MotionHistogramBucket,
} from "@/api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { ClipPlayer } from "@/components/clip-player";
import { Skeleton } from "@/components/ui/skeleton";
import { formatAgo } from "@/lib/format";

const RANGE_OPTIONS = [
  { label: "Last 1h", hours: 1, bucketSeconds: 60 },
  { label: "Last 6h", hours: 6, bucketSeconds: 5 * 60 },
  { label: "Last 24h", hours: 24, bucketSeconds: 60 * 60 },
  { label: "Last 7d", hours: 24 * 7, bucketSeconds: 6 * 60 * 60 },
] as const;

export function TimelinePage() {
  const camerasQuery = useQuery({
    queryKey: ["cameras", "list"],
    queryFn: listCameras,
    staleTime: 30_000,
  });

  // Depend on the query's `.data` directly (stable reference between
  // refetches) rather than the `cameras` fallback (which would create a
  // new `[]` every render and trip react-hooks/exhaustive-deps).
  const cameraList = camerasQuery.data;
  const cameras = cameraList ?? [];

  const [selectedCameraId, setSelectedCameraId] = useState<string | null>(
    null,
  );

  // Default to first camera once the list arrives.
  useEffect(() => {
    if (selectedCameraId === null && cameraList && cameraList.length > 0) {
      setSelectedCameraId(String(cameraList[0]!.id));
    }
  }, [cameraList, selectedCameraId]);

  const [rangeIdx, setRangeIdx] = useState(2); // last 24h default
  const range = RANGE_OPTIONS[rangeIdx]!;

  const { from, to } = useMemo(() => {
    const now = new Date();
    const fromDate = new Date(now.getTime() - range.hours * 3600 * 1000);
    return { from: fromDate.toISOString(), to: now.toISOString() };
    // Recomputed when range or selectedCameraId changes; downstream queries
    // are keyed on `from`/`to` so cache reuse is correct.
  }, [range]);

  const histogramQuery = useQuery({
    queryKey: [
      "cameras",
      selectedCameraId,
      "motion",
      "histogram",
      from,
      to,
      range.bucketSeconds,
    ],
    queryFn: () =>
      getCameraMotionHistogram(selectedCameraId!, {
        from,
        to,
        bucket_seconds: range.bucketSeconds,
      }),
    enabled: !!selectedCameraId,
    refetchInterval: 30_000,
  });

  const [focusBucket, setFocusBucket] =
    useState<MotionHistogramBucket | null>(null);

  // Clear focus if camera or range changes.
  useEffect(() => {
    setFocusBucket(null);
  }, [selectedCameraId, rangeIdx]);

  const buckets = histogramQuery.data ?? [];

  return (
    <div className="space-y-6">
      <header className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Timeline</h1>
          <p className="text-sm text-muted-foreground">
            Per-camera motion histogram. Click a bar to see its underlying
            events.
          </p>
        </div>
      </header>

      <Card>
        <CardHeader className="flex flex-row items-center justify-between gap-2 space-y-0 pb-3">
          <CardTitle className="flex items-center gap-2 text-sm font-medium">
            <Calendar className="h-4 w-4" />
            View
          </CardTitle>
        </CardHeader>
        <CardContent className="flex flex-wrap items-end gap-4">
          <div className="space-y-1">
            <label
              htmlFor="camera-select"
              className="text-xs text-muted-foreground"
            >
              Camera
            </label>
            <select
              id="camera-select"
              className="h-9 rounded-md border border-border bg-background px-2 text-sm"
              value={selectedCameraId ?? ""}
              onChange={(e) => setSelectedCameraId(e.target.value || null)}
            >
              {cameras.length === 0 ? (
                <option value="">No cameras</option>
              ) : null}
              {cameras.map((cam) => (
                <option key={String(cam.id)} value={String(cam.id)}>
                  {cam.name ?? String(cam.id)}
                </option>
              ))}
            </select>
          </div>
          <div className="space-y-1">
            <span className="text-xs text-muted-foreground">Range</span>
            <div className="flex gap-1">
              {RANGE_OPTIONS.map((opt, i) => (
                <button
                  key={opt.label}
                  type="button"
                  onClick={() => setRangeIdx(i)}
                  className={`rounded-md border px-2.5 py-1 text-xs transition ${
                    rangeIdx === i
                      ? "border-primary bg-primary/10 text-primary"
                      : "border-border text-muted-foreground hover:border-border/80"
                  }`}
                >
                  {opt.label}
                </button>
              ))}
            </div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="flex flex-row items-center justify-between gap-2 space-y-0 pb-2">
          <CardTitle className="flex items-center gap-2 text-sm font-medium">
            <BarChart3 className="h-4 w-4" />
            Motion histogram
          </CardTitle>
          <span className="text-xs text-muted-foreground">
            {buckets.length} non-empty buckets ·{" "}
            {buckets.reduce((s, b) => s + b.event_count, 0)} events
          </span>
        </CardHeader>
        <CardContent>
          {!selectedCameraId ? (
            <p className="py-8 text-center text-sm text-muted-foreground">
              Select a camera to view its motion histogram.
            </p>
          ) : histogramQuery.isLoading ? (
            <Skeleton className="h-40 w-full" />
          ) : histogramQuery.isError ? (
            <p className="py-8 text-center text-sm text-destructive">
              Failed to load histogram.
            </p>
          ) : buckets.length === 0 ? (
            <p className="py-8 text-center text-sm text-muted-foreground">
              No motion in this range.
            </p>
          ) : (
            <Histogram
              buckets={buckets}
              focusBucket={focusBucket}
              onSelect={setFocusBucket}
            />
          )}
        </CardContent>
      </Card>

      {focusBucket && selectedCameraId ? (
        <BucketDetail
          cameraId={selectedCameraId}
          bucket={focusBucket}
          bucketSeconds={range.bucketSeconds}
        />
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Histogram (inline SVG).
// ---------------------------------------------------------------------------

function Histogram({
  buckets,
  focusBucket,
  onSelect,
}: {
  buckets: MotionHistogramBucket[];
  focusBucket: MotionHistogramBucket | null;
  onSelect: (b: MotionHistogramBucket) => void;
}) {
  const max = Math.max(...buckets.map((b) => b.event_count), 1);
  const width = 800;
  const height = 160;
  const barGap = 1;
  const barWidth = Math.max(2, (width - buckets.length * barGap) / buckets.length);

  return (
    <div className="space-y-2">
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        className="h-40 w-full"
        role="img"
        aria-label="Motion histogram"
      >
        {buckets.map((b, i) => {
          const h = (b.event_count / max) * (height - 4);
          const x = i * (barWidth + barGap);
          const y = height - h;
          const focused = focusBucket?.bucket === b.bucket;
          return (
            <g key={b.bucket}>
              <rect
                x={x}
                y={0}
                width={barWidth}
                height={height}
                fill="transparent"
                onClick={() => onSelect(b)}
                style={{ cursor: "pointer" }}
              >
                <title>
                  {new Date(b.bucket_start).toLocaleString()} — {b.event_count}{" "}
                  events, {b.clip_count} clips
                </title>
              </rect>
              <rect
                x={x}
                y={y}
                width={barWidth}
                height={h}
                className={
                  focused
                    ? "fill-[hsl(var(--primary))]"
                    : "fill-[hsl(var(--primary)/0.55)]"
                }
                pointerEvents="none"
              />
            </g>
          );
        })}
      </svg>
      <div className="flex items-center justify-between text-xs text-muted-foreground">
        <span>{new Date(buckets[0]!.bucket_start).toLocaleString()}</span>
        <span>
          {new Date(buckets[buckets.length - 1]!.bucket_start).toLocaleString()}
        </span>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Bucket detail card.
// ---------------------------------------------------------------------------

function BucketDetail({
  cameraId,
  bucket,
  bucketSeconds,
}: {
  cameraId: string;
  bucket: MotionHistogramBucket;
  bucketSeconds: number;
}) {
  const from = bucket.bucket_start;
  const to = new Date(
    new Date(bucket.bucket_start).getTime() + bucketSeconds * 1000,
  ).toISOString();

  const motionQuery = useQuery({
    queryKey: ["cameras", cameraId, "motion", from, to],
    queryFn: () => listCameraMotion(cameraId, { from, to, limit: 500 }),
  });

  return (
    <Card>
      <CardHeader className="space-y-1 pb-3">
        <CardTitle className="text-base">
          {new Date(bucket.bucket_start).toLocaleString()}
        </CardTitle>
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <Badge variant="outline">{bucket.event_count} events</Badge>
          <Badge variant="outline">{bucket.clip_count} clips</Badge>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        {motionQuery.isLoading ? (
          <div className="space-y-2 p-4">
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
          </div>
        ) : (motionQuery.data ?? []).length === 0 ? (
          <p className="px-4 py-6 text-center text-sm text-muted-foreground">
            No motion rows in this bucket.
          </p>
        ) : (
          <div className="max-h-96 overflow-y-auto">
            <table className="w-full text-sm">
              <thead className="sticky top-0 bg-muted/30 text-xs uppercase text-muted-foreground">
                <tr>
                  <th className="px-3 py-2 text-left">When</th>
                  <th className="px-3 py-2 text-left">Kind</th>
                  <th className="px-3 py-2 text-left">Label</th>
                  <th className="px-3 py-2 text-left">Confidence</th>
                  <th className="px-3 py-2 text-left">Track</th>
                  <th className="px-3 py-2 text-left">Clip</th>
                </tr>
              </thead>
              <tbody>
                {(motionQuery.data ?? []).map((row) => (
                  <MotionRowEl key={row.id} row={row} />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function MotionRowEl({ row }: { row: MotionEventRow }) {
  const [playing, setPlaying] = useState(false);
  return (
    <>
      <tr className="border-t border-border/40">
        <td className="px-3 py-2 text-muted-foreground">
          {formatAgo(row.captured_at)}
        </td>
        <td className="px-3 py-2">
          <Badge
            variant={
              row.kind === "born"
                ? "success"
                : row.kind === "died"
                  ? "destructive"
                  : "secondary"
            }
            className="capitalize"
          >
            {row.kind}
          </Badge>
        </td>
        <td className="px-3 py-2 font-medium">{row.label}</td>
        <td className="px-3 py-2 font-mono">
          {(row.confidence * 100).toFixed(0)}%
        </td>
        <td className="px-3 py-2 font-mono text-muted-foreground">
          {row.track_id}
        </td>
        <td className="px-3 py-2 font-mono text-muted-foreground">
          {row.clip_id ? (
            <button
              type="button"
              onClick={() => setPlaying(true)}
              className="inline-flex items-center gap-1 rounded-md border border-border px-1.5 py-0.5 text-xs font-mono hover:bg-muted"
              aria-label={`Play clip ${row.clip_id}`}
            >
              <Play className="h-3 w-3" />#{row.clip_id}
            </button>
          ) : (
            "—"
          )}
        </td>
      </tr>
      {playing && row.clip_id ? (
        <ClipPlayerModal
          clipId={row.clip_id}
          title={`${row.label} · ${formatAgo(row.captured_at)}`}
          onClose={() => setPlaying(false)}
        />
      ) : null}
    </>
  );
}

// ---------------------------------------------------------------------------
// Modal clip player. Portal-less overlay; ESC closes.
// ---------------------------------------------------------------------------

function ClipPlayerModal({
  clipId,
  title,
  onClose,
}: {
  clipId: number;
  title: string;
  onClose: () => void;
}) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  return (
    <tr>
      <td colSpan={6} className="p-0">
        <div
          className="fixed inset-0 z-40 flex items-center justify-center bg-background/70 p-4 backdrop-blur-sm"
          role="dialog"
          aria-modal="true"
          onClick={onClose}
        >
          <div
            className="w-full max-w-3xl rounded-lg border border-border bg-background shadow-2xl"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between gap-2 border-b border-border px-4 py-3">
              <h3 className="truncate text-sm font-semibold">{title}</h3>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                onClick={onClose}
                aria-label="Close"
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
            <div className="p-4">
              <ClipPlayer clipId={clipId} />
            </div>
          </div>
        </div>
      </td>
    </tr>
  );
}
