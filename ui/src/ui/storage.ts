// M2.1 Stage B PR B5 — Local NVR storage view.
//
// Surface for the engine's on-disk clip storage. Two stacked sections:
//
//   1. Storage strip — recorder kind, panic state, free %, clips_dir.
//      Driven by GET /api/v1/storage/local. Renders unconditionally so
//      the operator always knows the recorder's runtime state.
//
//   2. Per-camera Timeline — for each configured camera, the last
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
  MotionEventRow,
  StorageLocalResponse,
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
  let body: StorageLocalResponse;
  try {
    body = await api.storage.local();
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

  const free = body.free_pct;
  const tone =
    body.panic ? "panic" : free != null && free < 15 ? "warn" : "ok";
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
        h("code", null, body.recorder_kind),
        body.panic
          ? h("span", { class: "panic-pill" }, "PANIC")
          : null,
      ),
      h(
        "div",
        { class: "storage-card-line" },
        h("span", { class: "metric" }, h("span", { class: "k" }, "Free"), h("span", null, freeLabel)),
        h("span", { class: "metric" }, h("span", { class: "k" }, "Path"), h("code", null, body.clips_dir)),
      ),
    ),
  );
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
