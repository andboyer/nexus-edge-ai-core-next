// M2.1 Stage B PR B5 — Local NVR storage view.
//
// Surface for the engine's on-disk clip storage. Three stacked sections:
//
//   1. Storage strip — recorder kind, panic state, free %, clips_dir.
//      Driven by GET /api/v1/storage. Renders unconditionally so the
//      operator always knows the recorder's runtime state.
//
//   2. Cold tier card — M2.2 Phase 5. Renders when a cold backend is
//      configured: handle + kind + health pill (Ok/ReadOnly/Unreachable
//      /NotRegistered) + pending/replicated counters. When health is
//      not Ok, an inline "Retry" button refetches the snapshot. A
//      separate >0 cold-only subtitle is shown above (and surfaces
//      even when cold is currently disabled, since cold-only clips
//      can survive a backend deconfigure).
//
//   3. Per-camera Timeline — for each configured camera, the last
//      hour of motion events collapsed by clip_id (most recent first).
//      Each clip tile shows the engine-generated 320px JPEG
//      thumbnail (GET /api/v1/clips/:id/thumbnail) and click-opens
//      an inline <video> playing the clip via Range streaming
//      (GET /api/v1/clips/:id). Cameras with no motion in the
//      window render a "No recent motion" placeholder.
//
// Pre-roll is intentionally NOT visualised here — that lands as its
// own PR after Stage B closes. Same for hourly Timeline grids;
// keep B5 narrow so the API contract gets exercised end-to-end first.

import { api } from "../api/client.js";
import { clear, h } from "../lib/el.js";
import type {
  CameraConfig,
  CameraId,
  ClipId,
  ColdHealthOut,
  ColdStatus,
  MotionEventRow,
  StorageResponse,
} from "../api/types.js";

export async function renderStorage(root: HTMLElement): Promise<void> {
  clear(root);
  root.append(h("h2", null, "Storage"));

  const stripHost = h("div", { class: "storage-strip" });
  root.append(stripHost);
  void renderStorageStrip(stripHost);

  const camHost = h("div", { class: "storage-cameras" });
  root.append(camHost);

  let cameras: CameraConfig[] = [];
  try {
    cameras = await api.cameras.list();
  } catch (e) {
    camHost.append(
      h("p", { class: "muted" }, `Cameras unavailable: ${(e as Error).message}`),
    );
    return;
  }
  if (cameras.length === 0) {
    camHost.append(h("p", { class: "muted" }, "No cameras configured."));
    return;
  }

  // Render in the configured order so muscle memory matches the
  // Cameras tab.
  for (const cam of cameras) {
    const card = h("section", { class: "storage-camera" });
    card.append(
      h(
        "div",
        { class: "storage-camera-head" },
        h("h3", null, cam.name),
        h("span", { class: "muted mono" }, cam.url),
      ),
    );
    const stripWrap = h("div", { class: "clip-strip" });
    card.append(stripWrap);
    camHost.append(card);
    void renderClipsForCamera(stripWrap, cam.id);
  }
}

async function renderStorageStrip(host: HTMLElement): Promise<void> {
  clear(host);
  let body: StorageResponse;
  try {
    body = await api.storage.full();
  } catch (e) {
    host.append(
      h(
        "div",
        { class: "storage-card warn" },
        h("strong", null, "Storage status unavailable"),
        h("span", { class: "muted" }, ` — ${(e as Error).message}`),
      ),
    );
    return;
  }

  const hot = body.hot;
  const free = hot.free_pct;
  const tone =
    hot.panic ? "panic" : free != null && free < 15 ? "warn" : "ok";
  const freeLabel = free != null ? `${free.toFixed(1)}%` : "—";

  host.append(
    h(
      "div",
      { class: `storage-card ${tone}` },
      h(
        "div",
        { class: "storage-card-head" },
        h("span", { class: `dot dot-${tone === "ok" ? "ok" : tone === "warn" ? "warn" : "crit"}` }),
        h("strong", null, "Local NVR"),
        h("span", { class: "muted" }, ` · recorder = `),
        h("code", null, hot.recorder_kind),
        hot.panic
          ? h("span", { class: "panic-pill" }, "PANIC")
          : null,
      ),
      h(
        "div",
        { class: "storage-card-line" },
        h("span", { class: "metric" }, h("span", { class: "k" }, "Free"), h("span", null, freeLabel)),
        h("span", { class: "metric" }, h("span", { class: "k" }, "Path"), h("code", null, hot.clips_dir)),
        body.cold_only_count > 0
          ? h(
              "span",
              { class: "metric muted" },
              h("span", { class: "k" }, "Cold-only"),
              h("span", null, `${body.cold_only_count} clip${body.cold_only_count === 1 ? "" : "s"}`),
            )
          : null,
      ),
    ),
  );

  // Cold-tier card. Renders when a cold backend is configured;
  // falls back to a single-line muted hint when not (so the
  // operator knows the surface exists and where to enable it).
  host.append(renderColdCard(body.cold, () => void renderStorageStrip(host)));
}

function renderColdCard(
  cold: ColdStatus | null,
  retry: () => void,
): HTMLElement {
  if (cold == null) {
    return h(
      "div",
      { class: "storage-card" },
      h(
        "div",
        { class: "storage-card-head" },
        h("span", { class: "dot dot-ok" }),
        h("strong", null, "Cold replication"),
        h("span", { class: "muted" }, " · disabled"),
      ),
      h(
        "div",
        { class: "storage-card-line muted" },
        h("span", null, "Configure a cold backend in the Storage Admin tab to mirror clips off-box."),
      ),
    );
  }

  const { tone, label, reason } = coldHealthVisual(cold.health);
  const head = h(
    "div",
    { class: "storage-card-head" },
    h("span", { class: `dot dot-${tone}` }),
    h("strong", null, "Cold replication"),
    h("span", { class: "muted" }, ` · backend `),
    h("code", null, cold.handle),
    h("span", { class: "muted" }, ` (${cold.kind})`),
    h("span", { class: `health-pill health-${tone}` }, label),
  );
  if (tone !== "ok") {
    head.append(
      h(
        "button",
        {
          class: "ghost",
          on: { click: retry },
          title: "Re-fetch /api/v1/storage to re-probe backend health",
        },
        "Retry",
      ),
    );
  }

  const metrics = h(
    "div",
    { class: "storage-card-line" },
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Pending"),
      h("span", null, String(cold.pending_count)),
    ),
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Replicated"),
      h("span", null, String(cold.replicated_count)),
    ),
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Cold-only"),
      h("span", null, String(cold.cold_only_count)),
    ),
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Uploaded"),
      h("span", null, formatBytes(cold.lifetime_uploaded_bytes)),
    ),
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Throttle"),
      h("span", null, formatBps(cold.throttle_bps)),
    ),
  );

  const card = h(
    "div",
    { class: `storage-card ${tone === "ok" ? "" : tone === "warn" ? "warn" : "panic"}` },
    head,
    metrics,
  );
  if (reason) {
    card.append(
      h(
        "div",
        { class: "storage-card-line muted" },
        h("strong", null, "Reason: "),
        h("span", null, reason),
        // Pending-while-down hint: the watermark sweeper can still
        // soft-evict hot clips while the cold backend is down, so
        // anything in `pending_count` is at risk of disappearing
        // locally before it ever uploads.
        cold.pending_count > 0
          ? h(
              "span",
              null,
              ` · ${cold.pending_count} clip${cold.pending_count === 1 ? "" : "s"} queued; watermark eviction may evict locally before upload completes.`,
            )
          : null,
      ),
    );
  }
  return card;
}

function coldHealthVisual(health: ColdHealthOut): {
  tone: "ok" | "warn" | "crit";
  label: string;
  reason: string | null;
} {
  switch (health.status) {
    case "ok":
      return { tone: "ok", label: "Ok", reason: null };
    case "read_only":
      return { tone: "warn", label: "Read-only", reason: health.reason };
    case "unreachable":
      return { tone: "crit", label: "Unreachable", reason: health.reason };
    case "not_registered":
      return {
        tone: "crit",
        label: "Not registered",
        reason:
          "The configured backend handle did not load at boot. Re-create it in the Storage Admin tab.",
      };
  }
}

function formatBps(bps: number): string {
  if (bps <= 0) return "unthrottled";
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} MB/s`;
  if (bps >= 1_000) return `${(bps / 1_000).toFixed(1)} kB/s`;
  return `${bps} B/s`;
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}

async function renderClipsForCamera(
  host: HTMLElement,
  cameraId: CameraId,
): Promise<void> {
  clear(host);
  let rows: MotionEventRow[];
  try {
    rows = await api.motion.listForCamera(cameraId, { limit: 500 });
  } catch (e) {
    host.append(
      h(
        "p",
        { class: "muted" },
        `Motion unavailable: ${(e as Error).message}`,
      ),
    );
    return;
  }

  // Collapse motion_events to one entry per clip_id (the row
  // payload is per-event, but we only need a clip preview here).
  // Most-recent-first: rows already arrive sorted DESC by
  // captured_at from the engine.
  const clips: { id: ClipId; capturedAt: string; eventCount: number }[] = [];
  const seen = new Map<ClipId, number>();
  for (const r of rows) {
    const idx = seen.get(r.clip_id);
    if (idx == null) {
      seen.set(r.clip_id, clips.length);
      clips.push({ id: r.clip_id, capturedAt: r.captured_at, eventCount: 1 });
    } else {
      clips[idx]!.eventCount += 1;
    }
  }

  if (clips.length === 0) {
    host.append(h("p", { class: "muted" }, "No recent motion."));
    return;
  }

  const grid = h("div", { class: "clip-grid" });
  for (const c of clips.slice(0, 24)) {
    const img = h("img", {
      src: api.clips.thumbnailUrl(c.id),
      alt: `Clip ${c.id}`,
      loading: "lazy",
    });
    // Hide the broken-image icon when the engine returns 503
    // (stub recorder, file still being written, etc).
    img.addEventListener("error", () => {
      img.style.opacity = "0.2";
    });
    const tile = h(
      "button",
      {
        class: "clip-tile",
        title: `${c.eventCount} motion event${c.eventCount === 1 ? "" : "s"} · ${c.capturedAt}`,
        on: {
          click: () => openClipModal(c.id),
        },
      },
      img,
      h("span", { class: "clip-ts" }, formatTs(c.capturedAt)),
    );
    grid.append(tile);
  }
  host.append(grid);
}

function openClipModal(clipId: ClipId): void {
  const overlay = h("div", { class: "clip-modal-overlay" });
  const close = (): void => {
    overlay.remove();
    document.removeEventListener("keydown", onKey);
  };
  const onKey = (ev: KeyboardEvent): void => {
    if (ev.key === "Escape") close();
  };
  overlay.addEventListener("click", (ev) => {
    if (ev.target === overlay) close();
  });
  document.addEventListener("keydown", onKey);

  const video = h("video", {
    src: api.clips.streamUrl(clipId),
    controls: true,
    autoplay: true,
    playsInline: true,
    class: "clip-modal-video",
  });
  // Surface fetch failures (404, 503 stub, etc) as visible text
  // instead of a black silent player.
  video.addEventListener("error", () => {
    const err = video.error;
    const msg = err
      ? `Playback failed (code ${err.code}: ${err.message || "unknown"})`
      : "Playback failed";
    const banner = h("div", { class: "clip-modal-error" }, msg);
    video.replaceWith(banner);
  });

  const card = h(
    "div",
    { class: "clip-modal-card" },
    h(
      "div",
      { class: "clip-modal-head" },
      h("strong", null, `Clip ${clipId}`),
      h(
        "button",
        {
          class: "ghost",
          on: { click: close },
        },
        "Close",
      ),
    ),
    video,
  );
  overlay.append(card);
  document.body.append(overlay);
}

function formatTs(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
