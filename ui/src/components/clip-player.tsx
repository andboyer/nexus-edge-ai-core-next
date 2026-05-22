// Reusable clip player.
//
// Wraps `<video src={clipUrl(...)}>` with two affordances the bare
// element lacks:
//
//   1. Pre-flight probe (Range: bytes=0-0) so we surface the engine's
//      JSON error body inline. The most common production failure is
//      `recorder=stub` (503) — the bare `<video>` element renders the
//      browser's "no source" stub, which is silently misleading. With
//      the probe we render "Playback unavailable — recorder=stub" so
//      operators know to flip `[runtime.clips] recorder = "gstreamer"`
//      in their TOML.
//
//   2. `onError` fallback covering codec/container issues that pass
//      the probe but break inside the media element (e.g. fragmented
//      MP4 with no moov yet, mid-write file).
//
// Used from both the alert detail drawer (`pages/events.tsx`) and the
// timeline bucket detail (`pages/timeline.tsx`).

import { AlertCircle } from "lucide-react";
import { useEffect, useState } from "react";

import { ApiError } from "@/api/client";
import { clipUrl } from "@/api/system";
import { Skeleton } from "@/components/ui/skeleton";

interface ProbeState {
  status: "probing" | "ok" | "error";
  error?: string;
  reason?: string;
  httpStatus?: number;
}

export function ClipPlayer({
  clipId,
  className,
}: {
  clipId: number | string;
  className?: string;
}) {
  const [state, setState] = useState<ProbeState>({ status: "probing" });
  const [mediaError, setMediaError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setState({ status: "probing" });
    setMediaError(null);

    // Cheapest legitimate request that still triggers the same error
    // path as a video element opening the stream: a 1-byte Range.
    fetch(clipUrl(clipId), {
      method: "GET",
      headers: { Range: "bytes=0-0" },
    })
      .then(async (res) => {
        if (cancelled) return;
        if (res.ok || res.status === 206) {
          setState({ status: "ok" });
          return;
        }
        let body: unknown = null;
        try {
          body = await res.json();
        } catch {
          // Not JSON — fall back to the status line.
        }
        const errMsg =
          body && typeof body === "object" && "error" in body
            ? String((body as { error: unknown }).error)
            : `HTTP ${res.status}`;
        const reason =
          body && typeof body === "object" && "reason" in body
            ? String((body as { reason: unknown }).reason)
            : undefined;
        setState({
          status: "error",
          error: errMsg,
          reason,
          httpStatus: res.status,
        });
      })
      .catch((e) => {
        if (cancelled) return;
        const msg =
          e instanceof ApiError ? e.message : (e as Error)?.message ?? "fetch failed";
        setState({ status: "error", error: msg });
      });

    return () => {
      cancelled = true;
    };
  }, [clipId]);

  if (state.status === "probing") {
    return <Skeleton className={`aspect-video w-full ${className ?? ""}`} />;
  }

  if (state.status === "error") {
    return <ClipUnavailable state={state} className={className} />;
  }

  return (
    <>
      <video
        key={String(clipId)}
        controls
        preload="metadata"
        src={clipUrl(clipId)}
        onError={(e) => {
          const v = e.currentTarget;
          // MediaError.code: 1=ABORTED, 2=NETWORK, 3=DECODE, 4=SRC_NOT_SUPPORTED.
          const code = v.error?.code;
          setMediaError(
            v.error?.message ||
              (code
                ? `media error ${code}`
                : "Playback failed (unknown media error)."),
          );
        }}
        className={`w-full rounded-md border border-border bg-black ${
          className ?? ""
        }`}
      />
      {mediaError ? (
        <p className="mt-2 flex items-center gap-1.5 text-xs text-destructive">
          <AlertCircle className="h-3.5 w-3.5" />
          {mediaError}
        </p>
      ) : null}
    </>
  );
}

function ClipUnavailable({
  state,
  className,
}: {
  state: ProbeState;
  className?: string;
}) {
  const recorderStub = state.reason === "recorder=stub";
  const noSamples = state.reason === "no_samples";
  return (
    <div
      className={`flex aspect-video w-full flex-col items-center justify-center gap-2 rounded-md border border-dashed border-border bg-muted/30 p-4 text-center ${
        className ?? ""
      }`}
    >
      <AlertCircle className="h-6 w-6 text-muted-foreground" />
      <p className="text-sm font-medium">Playback unavailable</p>
      <p className="text-xs text-muted-foreground">
        {state.error}
        {state.httpStatus ? (
          <span className="ml-1 font-mono opacity-70">
            ({state.httpStatus})
          </span>
        ) : null}
      </p>
      {recorderStub ? (
        <p className="max-w-prose text-xs text-muted-foreground">
          The engine is running with the <code>stub</code> clip recorder. Add{" "}
          <code>recorder = &quot;gstreamer&quot;</code> under{" "}
          <code>[runtime.clips]</code> in your TOML config and restart.
        </p>
      ) : noSamples ? (
        <p className="max-w-prose text-xs text-muted-foreground">
          The recorder closed this clip before any video frames were
          written (camera stalled or all buffers were dropped). The
          file on disk is a header-only MP4 stub with no playable
          data.
        </p>
      ) : state.reason ? (
        <p className="text-xs text-muted-foreground">
          reason: <code>{state.reason}</code>
        </p>
      ) : null}
    </div>
  );
}
