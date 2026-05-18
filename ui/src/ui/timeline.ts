// M2.1 Stage B PR B7 — per-camera Timeline grid.
//
// Three-layer view, top-down:
//
//   1. Header — title + window label ("Last 24h, 1h buckets").
//
//   2. Per-camera row — 24 hour-cells horizontally, each cell
//      coloured by motion-event density (sparse bars from the
//      engine's GET /v1/cameras/:id/motion/histogram). Empty hours
//      render as a dim placeholder so the grid stays time-aligned
//      even on quiet cameras.
//
//   3. Drawer — clicking a cell expands an inline panel below the
//      row listing every clip touched in that hour (deduped from
//      GET /v1/cameras/:id/motion?from=&to=). Each clip is a
//      thumbnail tile that opens the same click-to-play modal as
//      the Storage tab (we mount our own copy here so this tab can
//      ship independently of B5's storage.ts).
//
// All errors silent (engine offline / no motion → empty cells)
// matching the project-wide try/catch convention. The histogram
// endpoint is cheap (24 datapoints × N cameras) so we re-fetch on
// every tab activation; no caching layer.

import { api } from "../api/client.js";
import { clear, h } from "../lib/el.js";
import type {
  CameraConfig,
  CameraId,
  ClipId,
  MotionEventRow,
  MotionHistogramBucket,
} from "../api/types.js";

const HOURS = 24;
const BUCKET_SECONDS = 3600;

export async function renderTimeline(root: HTMLElement): Promise<void> {
  clear(root);
  root.append(h("h2", null, "Timeline"));
  root.append(
    h(
      "p",
      { class: "muted" },
      `Last ${HOURS} hours, ${BUCKET_SECONDS / 60}-minute buckets. Click an hour to list its clips.`,
    ),
  );

  // Anchor the grid so the LAST cell is the current (in-progress)
  // hour. We snap `now` down to the hour boundary, then push `to`
  // one bucket forward so the [from, to) window covers
  // [now-23h, now+1h) — i.e. 24 cells whose final bucket is the
  // hour the operator is currently looking at. Without this push
  // the current hour silently falls off the right edge.
  const now = new Date();
  now.setMinutes(0, 0, 0);
  const to = new Date(now.getTime() + BUCKET_SECONDS * 1000);
  const from = new Date(to.getTime() - HOURS * BUCKET_SECONDS * 1000);

  const host = h("div", { class: "timeline-host" });
  root.append(host);

  // Header row of hour labels (sparse, every 4h, to keep it
  // readable on narrow viewports).
  host.append(renderHourScale(from));

  let cameras: CameraConfig[] = [];
  try {
    cameras = await api.cameras.list();
  } catch (e) {
    host.append(
      h("p", { class: "muted" }, `Cameras unavailable: ${(e as Error).message}`),
    );
    return;
  }
  if (cameras.length === 0) {
    host.append(h("p", { class: "muted" }, "No cameras configured."));
    return;
  }

  for (const cam of cameras) {
    host.append(renderCameraRow(cam, from, to));
  }
}

function renderHourScale(from: Date): HTMLElement {
  const scale = h("div", { class: "timeline-scale" });
  scale.append(h("div", { class: "timeline-scale-label muted" }, "Camera"));
  const cells = h("div", { class: "timeline-scale-cells" });
  for (let i = 0; i < HOURS; i++) {
    const ts = new Date(from.getTime() + i * BUCKET_SECONDS * 1000);
    // Label every 4th hour to keep the bar from getting noisy.
    const label = i % 4 === 0 ? String(ts.getHours()).padStart(2, "0") : "";
    cells.append(h("div", { class: "timeline-scale-cell muted" }, label));
  }
  scale.append(cells);
  return scale;
}

function renderCameraRow(
  cam: CameraConfig,
  from: Date,
  to: Date,
): HTMLElement {
  const row = h("section", { class: "timeline-row" });
  row.append(
    h(
      "div",
      { class: "timeline-row-head" },
      h("strong", null, cam.name),
      h("span", { class: "muted mono" }, `#${cam.id}`),
    ),
  );

  const cells = h("div", { class: "timeline-cells" });
  // Pre-populate empty cells so the grid is laid out immediately;
  // the histogram fetch fills counts in-place.
  const cellEls: HTMLButtonElement[] = [];
  for (let i = 0; i < HOURS; i++) {
    const cellTs = new Date(from.getTime() + i * BUCKET_SECONDS * 1000);
    const cell = h(
      "button",
      {
        class: "timeline-cell density-0",
        title: `${formatBucketTitle(cellTs)} — loading…`,
        on: {
          click: () => onCellClick(row, cam.id, cellTs),
        },
      },
      h("span", { class: "timeline-cell-count" }, ""),
    );
    cells.append(cell);
    cellEls.push(cell as HTMLButtonElement);
  }
  row.append(cells);

  // Drawer host (filled when a cell is clicked).
  row.append(h("div", { class: "timeline-drawer" }));

  // Fetch histogram for this camera; fold counts into the right
  // cells. Errors silent — empty grid is the fallback.
  void (async () => {
    let buckets: MotionHistogramBucket[] = [];
    try {
      buckets = await api.motion.histogramForCamera(cam.id, {
        from: from.toISOString(),
        to: to.toISOString(),
        bucket_seconds: BUCKET_SECONDS,
      });
    } catch {
      // leave grid at density-0
      for (const cell of cellEls) {
        cell.title = `${cell.title.split(" — ")[0]} — engine unreachable`;
      }
      return;
    }
    // Engine returns sparse buckets — index by `bucket` field.
    const max = buckets.reduce((m, b) => Math.max(m, b.event_count), 0);
    for (let i = 0; i < HOURS; i++) {
      const cellTs = new Date(from.getTime() + i * BUCKET_SECONDS * 1000);
      const b = buckets.find((x) => x.bucket === i);
      const cell = cellEls[i]!;
      if (!b || b.event_count === 0) {
        cell.title = `${formatBucketTitle(cellTs)} — no motion`;
        continue;
      }
      cell.classList.remove("density-0");
      cell.classList.add(`density-${densityBucket(b.event_count, max)}`);
      cell.title = `${formatBucketTitle(cellTs)} — ${b.event_count} event${b.event_count === 1 ? "" : "s"} across ${b.clip_count} clip${b.clip_count === 1 ? "" : "s"}`;
      const count = cell.querySelector(".timeline-cell-count");
      if (count) count.textContent = String(b.clip_count);
    }
  })();

  return row;
}

function densityBucket(count: number, max: number): 1 | 2 | 3 | 4 {
  if (max <= 0) return 1;
  const ratio = count / max;
  if (ratio < 0.25) return 1;
  if (ratio < 0.5) return 2;
  if (ratio < 0.75) return 3;
  return 4;
}

function formatBucketTitle(ts: Date): string {
  const hh = String(ts.getHours()).padStart(2, "0");
  const day = ts.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
  return `${day} ${hh}:00`;
}

async function onCellClick(
  row: HTMLElement,
  cameraId: CameraId,
  cellTs: Date,
): Promise<void> {
  const drawer = row.querySelector<HTMLElement>(".timeline-drawer");
  if (!drawer) return;
  // Toggle: if this drawer is already open for the same hour, close it.
  const openedFor = drawer.dataset.bucket;
  const cellKey = String(cellTs.getTime());
  if (openedFor === cellKey) {
    clear(drawer);
    delete drawer.dataset.bucket;
    return;
  }
  drawer.dataset.bucket = cellKey;
  clear(drawer);
  drawer.append(h("p", { class: "muted" }, "Loading clips…"));

  const from = cellTs.toISOString();
  const to = new Date(cellTs.getTime() + BUCKET_SECONDS * 1000).toISOString();

  let events: MotionEventRow[] = [];
  try {
    events = await api.motion.listForCamera(cameraId, {
      from,
      to,
      limit: 5000,
    });
  } catch (e) {
    clear(drawer);
    drawer.append(
      h("p", { class: "muted" }, `Motion unavailable: ${(e as Error).message}`),
    );
    return;
  }

  // Dedupe to one entry per clip_id, preserving first-seen captured_at.
  const clips: { id: ClipId; capturedAt: string; eventCount: number }[] = [];
  const seen = new Map<ClipId, number>();
  for (const ev of events) {
    const idx = seen.get(ev.clip_id);
    if (idx == null) {
      seen.set(ev.clip_id, clips.length);
      clips.push({
        id: ev.clip_id,
        capturedAt: ev.captured_at,
        eventCount: 1,
      });
    } else {
      clips[idx]!.eventCount += 1;
    }
  }

  clear(drawer);
  drawer.append(
    h(
      "div",
      { class: "timeline-drawer-head" },
      h("strong", null, formatBucketTitle(cellTs)),
      h(
        "span",
        { class: "muted" },
        ` — ${clips.length} clip${clips.length === 1 ? "" : "s"}, ${events.length} event${events.length === 1 ? "" : "s"}`,
      ),
    ),
  );

  if (clips.length === 0) {
    drawer.append(h("p", { class: "muted" }, "No clips in this hour."));
    return;
  }

  const grid = h("div", { class: "clip-grid" });
  for (const c of clips) {
    const img = h("img", {
      src: api.clips.thumbnailUrl(c.id),
      alt: `Clip ${c.id}`,
      loading: "lazy",
    });
    img.addEventListener("error", () => {
      img.style.opacity = "0.2";
    });
    const tile = h(
      "button",
      {
        class: "clip-tile",
        title: `${c.eventCount} event${c.eventCount === 1 ? "" : "s"} · ${c.capturedAt}`,
        on: {
          click: () => openClipModal(c.id),
        },
      },
      img,
      h("span", { class: "clip-ts" }, formatTs(c.capturedAt)),
    );
    grid.append(tile);
  }
  drawer.append(grid);
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
