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
  ClipTracksResponse,
  MotionEventRow,
  MotionHistogramBucket,
  TrackId,
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

/// Options for [`openClipModal`].
///
/// `focusTrackId` narrows the bbox overlay to a single track id —
/// used when the modal is opened from a specific alert card so
/// reviewers see ONLY the object that triggered that alert. When
/// omitted, every track that fired an alert on the clip is drawn
/// (the legacy behaviour used by storage-tab thumbnail clicks).
///
/// Background: a single clip can carry multiple alerts on
/// different tracks (e.g. a car-loitering alert AND a separate
/// person alert on a mislabeled sign). Without a focus filter,
/// the overlay drew every triggered track, making it impossible
/// to tell which box belonged to which alert.
export interface OpenClipModalOpts {
  focusTrackId?: TrackId;
}

/// Open the full clip viewer with bbox-overlay support. Exported so
/// other tabs (the live-alert ticker, future "deep-link to event"
/// surfaces) can reuse the player without re-implementing the
/// overlay/scaling plumbing. Storage tab still has its own simpler
/// modal for the grid view — intentional, that one doesn't need
/// overlays.
export function openClipModal(
  clipId: ClipId,
  opts: OpenClipModalOpts = {},
): void {
  const overlay = h("div", { class: "clip-modal-overlay" });
  const close = (): void => {
    overlayState.stop();
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
    overlayState.stop();
  });

  // Canvas overlay sits absolutely on top of the <video> inside
  // a positioned wrapper. pointer-events:none keeps the native
  // <video> controls clickable underneath.
  const canvas = h("canvas", {
    class: "clip-modal-overlay-canvas",
  });
  const stage = h(
    "div",
    { class: "clip-modal-stage" },
    video,
    canvas,
  );

  // Per-modal mutable state owned by the controller below. Defined
  // up here so `close()` can `.stop()` the rAF loop on dismiss.
  const overlayState = createOverlayController(clipId, video, canvas, {
    ...(opts.focusTrackId !== undefined
      ? { focusTrackId: opts.focusTrackId }
      : {}),
  });

  // Toggle pill — defaults to ON; remembers preference in
  // localStorage so an operator who hates the overlay only flips
  // it once per browser.
  const PREF_KEY = "nexus.clip.overlay";
  const initial = window.localStorage.getItem(PREF_KEY) !== "0";
  const toggle = h(
    "button",
    {
      class: "ghost",
      title: "Show / hide bounding-box overlay",
      on: {
        click: () => {
          const next = !overlayState.enabled();
          overlayState.setEnabled(next);
          window.localStorage.setItem(PREF_KEY, next ? "1" : "0");
          toggle.textContent = next ? "Hide boxes" : "Show boxes";
        },
      },
    },
    initial ? "Hide boxes" : "Show boxes",
  );
  overlayState.setEnabled(initial);

  const card = h(
    "div",
    { class: "clip-modal-card" },
    h(
      "div",
      { class: "clip-modal-head" },
      h("strong", null, `Clip ${clipId}`),
      h(
        "div",
        { class: "clip-modal-actions" },
        toggle,
        h(
          "button",
          {
            class: "ghost",
            on: { click: close },
          },
          "Close",
        ),
      ),
    ),
    stage,
  );
  overlay.append(card);
  document.body.append(overlay);
}

// -----------------------------------------------------------------
// Overlay controller — owns the fetch, the draw loop, and the
// keep-canvas-aligned-with-video resize logic. Pulled out of
// `openClipModal` so the modal body reads top-to-bottom without
// the noisy implementation details.
//
// Tracks rendered:
//   * Group `events` by `track_id`. A track is "alive" from its
//     first event (usually `born`) until its `died` event (or to
//     the end of the clip if no `died` row exists).
//   * At each animation frame, currentTimeMs = video.currentTime
//     * 1000. Wall-clock = clip.started_at + currentTimeMs. For
//     each track alive at that wall-clock, pick the most recent
//     event with `captured_at <= wall-clock` and draw its bbox.
//   * Colour is derived from `track_id` so the same object keeps
//     the same colour across `updated` events.
//
// Coordinate scaling:
//   * bbox lives in supervisor-frame pixels (currently a fixed
//     960×540 RGB set by RtspSource's videoscale caps; published
//     as `source_width`/`source_height` on the API response).
//   * Canvas backing buffer is locked to `video.videoWidth /
//     videoHeight` (the intrinsic resolution of the decoded MP4)
//     AFTER `loadedmetadata` fires. CSS sizes the canvas to match
//     the <video> element's render box, so the browser handles
//     letterboxing automatically.
//   * Per draw call we multiply each bbox coordinate by
//     (videoWidth/source_width, videoHeight/source_height) so the
//     boxes land on the actual objects. Without this scale the
//     boxes appear at ~half size in the top-left quadrant on any
//     1080p camera (because 960/1920 = 0.5).
// -----------------------------------------------------------------
interface OverlayController {
  stop(): void;
  setEnabled(on: boolean): void;
  enabled(): boolean;
}

interface OverlayControllerOpts {
  /// When set, the overlay draws ONLY events for this track id
  /// (regardless of how many trigger tracks the clip has). See
  /// [`OpenClipModalOpts.focusTrackId`].
  focusTrackId?: TrackId;
}

function createOverlayController(
  clipId: ClipId,
  video: HTMLVideoElement,
  canvas: HTMLCanvasElement,
  controllerOpts: OverlayControllerOpts = {},
): OverlayController {
  let on = true;
  let raf: number | null = null;
  let stopped = false;
  // Indexed lookup populated once the fetch resolves.
  let tracks: TrackTimeline[] = [];
  let startedAtMs = 0;
  // Pixel dimensions of the coordinate space `bbox` values live
  // in. Defaults to 1 so the multiplier maths is a no-op before
  // the API responds (no boxes are drawn in that window either,
  // because `tracks` is empty).
  let sourceWidth = 1;
  let sourceHeight = 1;
  const ctx = canvas.getContext("2d");

  // Fetch the overlay rows. Failure is silent — empty `tracks`
  // means the loop runs but draws nothing, exactly matching
  // "clip has no motion_events" (a legitimate state).
  //
  // `trigger_track_ids` filters out any track that didn't trigger
  // an alert on this clip, so reviewers only see the object that
  // tripped the rule. If the API returns an empty list (motion-only
  // clip with no rule fires, or a legacy clip pre-dating the
  // events.track_id stamping), we draw nothing — see the
  // `ClipTracksResponse` doc on the engine side for the rationale.
  api.clips
    .tracks(clipId)
    .then((resp: ClipTracksResponse) => {
      startedAtMs = Date.parse(resp.clip.started_at);
      sourceWidth = resp.source_width > 0 ? resp.source_width : 1;
      sourceHeight = resp.source_height > 0 ? resp.source_height : 1;
      // Focus-track mode: ignore the clip-wide trigger set and
      // draw ONLY the requested track. The supervisor stamps the
      // alert's `track_id` onto every events row, so callers that
      // opened the modal from a specific alert can pin the overlay
      // to exactly that object even when the clip carries multiple
      // unrelated alerts (e.g. a car alert + a person alert on a
      // mislabeled sign on the same clip).
      const allowed: Set<TrackId> =
        controllerOpts.focusTrackId !== undefined
          ? new Set<TrackId>([controllerOpts.focusTrackId])
          : new Set<TrackId>(resp.trigger_track_ids);
      const filtered = allowed.size === 0
        ? []
        : resp.events.filter((e) => allowed.has(e.track_id));
      tracks = buildTrackTimelines(filtered);
    })
    .catch(() => {
      tracks = [];
    });

  // Re-size the backing buffer to match the video's intrinsic
  // resolution once it's known. We rebind on every metadata event
  // because re-seeking can change videoWidth on adaptive streams.
  const syncCanvasSize = (): void => {
    const w = video.videoWidth;
    const h = video.videoHeight;
    if (w <= 0 || h <= 0) return;
    if (canvas.width !== w) canvas.width = w;
    if (canvas.height !== h) canvas.height = h;
  };
  video.addEventListener("loadedmetadata", syncCanvasSize);
  video.addEventListener("resize", syncCanvasSize);

  const draw = (): void => {
    if (stopped) return;
    raf = window.requestAnimationFrame(draw);
    if (!ctx || !on || canvas.width === 0) return;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    if (tracks.length === 0 || startedAtMs === 0) return;
    const scaleX = canvas.width / sourceWidth;
    const scaleY = canvas.height / sourceHeight;
    const wallMs = startedAtMs + video.currentTime * 1000;
    for (const t of tracks) {
      if (wallMs < t.startMs || wallMs > t.endMs) continue;
      const ev = pickEventAt(t.events, wallMs);
      if (!ev) continue;
      drawBox(ctx, ev, t.colour, scaleX, scaleY);
    }
  };
  raf = window.requestAnimationFrame(draw);

  return {
    stop(): void {
      stopped = true;
      if (raf !== null) window.cancelAnimationFrame(raf);
      raf = null;
    },
    setEnabled(next: boolean): void {
      on = next;
      if (!on && ctx) ctx.clearRect(0, 0, canvas.width, canvas.height);
    },
    enabled(): boolean {
      return on;
    },
  };
}

interface TrackTimeline {
  trackId: number;
  startMs: number;
  endMs: number;
  events: MotionEventRow[]; // ASC by captured_at
  colour: string;
}

function buildTrackTimelines(events: MotionEventRow[]): TrackTimeline[] {
  const groups = new Map<number, MotionEventRow[]>();
  for (const ev of events) {
    const arr = groups.get(ev.track_id);
    if (arr) arr.push(ev);
    else groups.set(ev.track_id, [ev]);
  }
  const out: TrackTimeline[] = [];
  for (const [trackId, evs] of groups) {
    // Defensive: groups only ever appends, so `evs` is non-empty,
    // but the type system can't prove that without these checks.
    if (evs.length === 0) continue;
    // Engine writes ASC already, but defend against future
    // changes to the SQL ORDER BY.
    evs.sort((a, b) => Date.parse(a.captured_at) - Date.parse(b.captured_at));
    const first = evs[0]!;
    const last = evs[evs.length - 1]!;
    const startMs = Date.parse(first.captured_at);
    // If the last event isn't a `died` row we leave the track
    // alive through the rest of the clip. Adding ~10 minutes
    // of grace is a defensive upper bound; the draw loop also
    // gates on `video.currentTime` so we never extrapolate
    // beyond the actual playback range.
    const endMs =
      last.kind === "died"
        ? Date.parse(last.captured_at)
        : Date.parse(last.captured_at) + 600_000;
    out.push({
      trackId,
      startMs,
      endMs,
      events: evs,
      colour: colourForTrack(trackId),
    });
  }
  return out;
}

/// Binary-search-ish lookup of the latest event with
/// `captured_at <= wallMs`. Linear scan is fine — a typical clip
/// has <100 events per track, and this runs once per track per
/// rAF tick (~60 Hz × N tracks ≈ negligible).
function pickEventAt(
  events: MotionEventRow[],
  wallMs: number,
): MotionEventRow | null {
  let best: MotionEventRow | null = null;
  for (const ev of events) {
    const t = Date.parse(ev.captured_at);
    if (t > wallMs) break;
    best = ev;
  }
  return best;
}

function drawBox(
  ctx: CanvasRenderingContext2D,
  ev: MotionEventRow,
  colour: string,
  scaleX: number,
  scaleY: number,
): void {
  const x1 = ev.bbox.x1 * scaleX;
  const y1 = ev.bbox.y1 * scaleY;
  const x2 = ev.bbox.x2 * scaleX;
  const y2 = ev.bbox.y2 * scaleY;
  const w = x2 - x1;
  const h = y2 - y1;
  if (w <= 0 || h <= 0) return;

  // Stroke
  ctx.lineWidth = Math.max(2, Math.round(ctx.canvas.width / 480));
  ctx.strokeStyle = colour;
  ctx.strokeRect(x1, y1, w, h);

  // Label pill above the box (or below if near the top edge).
  const text = `${ev.label} · ${Math.round(ev.confidence * 100)}%`;
  const fontPx = Math.max(12, Math.round(ctx.canvas.width / 90));
  ctx.font = `${fontPx}px system-ui, -apple-system, sans-serif`;
  const padX = Math.round(fontPx * 0.4);
  const padY = Math.round(fontPx * 0.25);
  const tw = ctx.measureText(text).width;
  const pillW = tw + padX * 2;
  const pillH = fontPx + padY * 2;
  const px = x1;
  let py = y1 - pillH;
  if (py < 0) py = y1; // fall below if there's no headroom
  ctx.fillStyle = colour;
  ctx.fillRect(px, py, pillW, pillH);
  ctx.fillStyle = "#000";
  ctx.textBaseline = "top";
  ctx.fillText(text, px + padX, py + padY);
}

/// Stable per-track colour. Hash the (u64) track_id down to one
/// of ~12 hues spread across the wheel so two adjacent tracks
/// almost always get visually distinct colours. Saturation +
/// lightness are fixed for legibility on dark video frames.
function colourForTrack(trackId: number): string {
  // JS bitwise ops are 32-bit; mod before bit-twiddling keeps the
  // hash deterministic even when the engine emits high u64 ids.
  const seed = ((trackId & 0xffffffff) ^ ((trackId / 0x100000000) | 0)) >>> 0;
  const hue = (seed * 47) % 360;
  return `hsl(${hue}, 85%, 60%)`;
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
