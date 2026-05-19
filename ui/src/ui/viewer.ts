import { api } from "../api/client.js";
import { clear, h } from "../lib/el.js";
import { drawFrame } from "../lib/canvas.js";
import type { CameraConfig, ZoneConfig } from "../api/types.js";

/// Page-wide preference for the static-filter diagnostic overlay.
/// Persisted in localStorage so an operator who turns it on while
/// tuning thresholds doesn't have to re-enable it every reload.
/// Mutating this also propagates to the modal because both poll
/// loops re-read it on every frame.
const DEBUG_STATIC_PREF_KEY = "nexus.viewer.debug_static";
let debugStaticEnabled = window.localStorage.getItem(DEBUG_STATIC_PREF_KEY) === "1";

/// Page-wide preference for the zone polygon overlay. Same
/// persistence + re-read-every-frame pattern as the static debug
/// toggle. Zones themselves are snapshotted when the viewer first
/// mounts (or the expand modal opens) — editing zones in the
/// camera form requires a page reload to see the new polygons in
/// the viewer.
const SHOW_ZONES_PREF_KEY = "nexus.viewer.show_zones";
let showZonesEnabled = window.localStorage.getItem(SHOW_ZONES_PREF_KEY) === "1";

export async function renderViewer(root: HTMLElement): Promise<void> {
  clear(root);

  // Header row: title on the left, toggle cluster on the right.
  const debugBtn = h(
    "button",
    {
      class: "ghost",
      title:
        "Show static-object filter state (EMA, dwell counters) under each vehicle box. " +
        "Helps diagnose why a moving vehicle was tagged static.",
      on: {
        click: () => {
          debugStaticEnabled = !debugStaticEnabled;
          window.localStorage.setItem(
            DEBUG_STATIC_PREF_KEY,
            debugStaticEnabled ? "1" : "0",
          );
          debugBtn.textContent = debugStaticEnabled
            ? "Hide static debug"
            : "Show static debug";
        },
      },
    },
    debugStaticEnabled ? "Hide static debug" : "Show static debug",
  );
  const zonesBtn = h(
    "button",
    {
      class: "ghost",
      title:
        "Overlay configured polygon zones on every live cell. " +
        "Inclusion = green, exclusion = red dashed, dwell = amber. " +
        "Zones are snapshotted at viewer load; reload after editing.",
      on: {
        click: () => {
          showZonesEnabled = !showZonesEnabled;
          window.localStorage.setItem(
            SHOW_ZONES_PREF_KEY,
            showZonesEnabled ? "1" : "0",
          );
          zonesBtn.textContent = showZonesEnabled
            ? "Hide zones"
            : "Show zones";
        },
      },
    },
    showZonesEnabled ? "Hide zones" : "Show zones",
  );
  root.append(
    h(
      "div",
      { class: "viewer-head" },
      h("h2", null, "Live viewer"),
      h("div", { class: "viewer-head-actions" }, zonesBtn, debugBtn),
    ),
  );

  const cams = await api.cameras.list();
  if (cams.length === 0) {
    root.append(h("p", { class: "muted" }, "No cameras configured."));
    return;
  }
  const grid = h("div", { class: "viewer-grid" });
  root.append(grid);
  for (const cam of cams) {
    const canvas = h("canvas", null);
    const expandBtn = h(
      "button",
      {
        class: "ghost viewer-expand-btn",
        title: "Expand to full resolution (Esc to close)",
        on: {
          click: () => openViewerModal(cam),
        },
      },
      // U+26F6 SQUARE FOUR CORNERS — universal "expand" affordance,
      // no icon font dependency.
      "⛶",
    );
    // `aria-label` isn't in the typed prop bag for `h()`, so set
    // it directly. Matches the pattern in lib/icons.ts.
    expandBtn.setAttribute("aria-label", `Expand ${cam.name} to full resolution`);
    const head = h(
      "div",
      { class: "viewer-cell-head" },
      h("h3", null, `${cam.name} (id ${cam.id})`),
      expandBtn,
    );
    const cell = h(
      "div",
      { class: "viewer-cell" },
      head,
      canvas,
    );
    grid.append(cell);
    void poll(cam.id, canvas, cam.zones ?? []);
  }
}

async function poll(
  cameraId: number,
  canvas: HTMLCanvasElement,
  zones: ReadonlyArray<ZoneConfig>,
) {
  const img = new Image();
  while (canvas.isConnected) {
    try {
      const meta = await api.cameras.latestMetadata(cameraId);
      img.src = api.cameras.latestSnapshotUrl(cameraId, meta.frame_id);
      await once(img, "load");
      drawFrame(
        canvas,
        img,
        meta.objects,
        { width: meta.width, height: meta.height },
        {
          debugStatic: debugStaticEnabled,
          ...(showZonesEnabled && zones.length > 0 ? { zones } : {}),
        },
      );
    } catch {
      // Engine restart, no frame yet, etc. Keep polling.
    }
    // 100ms => 10fps perceived ceiling. Server-side cap is the camera's
    // `max_fps`; if that's lower (e.g. 5) we just busy-poll the same
    // frame_id repeatedly, which is cheap because metadata is JSON.
    await sleep(100);
  }
}

/// Opens a fullscreen-ish modal that polls the same camera as the
/// grid cell but renders into a canvas sized to the viewport — so
/// the source snapshot is drawn at (up to) its native resolution
/// rather than the 240px-tall cell.
///
/// The modal sets `aspect-ratio` on the canvas from the first
/// metadata fetch so the stage doesn't reflow on every frame, then
/// `drawFrame` letterboxes inside it. Esc or click-on-backdrop
/// closes; the canvas removal terminates the poll loop because
/// `canvas.isConnected` flips false.
function openViewerModal(cam: CameraConfig): void {
  const overlay = h("div", { class: "viewer-modal-overlay" });

  const canvas = h("canvas", { class: "viewer-modal-canvas" });

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

  const closeBtn = h(
    "button",
    {
      class: "ghost",
      title: "Close (Esc)",
      on: { click: close },
    },
    "Close",
  );
  closeBtn.setAttribute("aria-label", "Close");

  const card = h(
    "div",
    { class: "viewer-modal-card" },
    h(
      "div",
      { class: "viewer-modal-head" },
      h("strong", null, `${cam.name} (id ${cam.id}) — live, full resolution`),
      closeBtn,
    ),
    h("div", { class: "viewer-modal-stage" }, canvas),
  );
  overlay.append(card);
  document.body.append(overlay);

  void pollModal(cam.id, canvas, cam.zones ?? []);
}

/// Same loop shape as `poll()` but applies the source dimensions
/// as an `aspect-ratio` CSS var on the parent stage so the canvas
/// fills as much of the viewport as it can without distorting.
async function pollModal(
  cameraId: number,
  canvas: HTMLCanvasElement,
  zones: ReadonlyArray<ZoneConfig>,
) {
  const img = new Image();
  let aspectApplied = false;
  while (canvas.isConnected) {
    try {
      const meta = await api.cameras.latestMetadata(cameraId);
      img.src = api.cameras.latestSnapshotUrl(cameraId, meta.frame_id);
      await once(img, "load");
      if (!aspectApplied && meta.width > 0 && meta.height > 0) {
        // Set aspect-ratio on the stage wrapper so CSS can size
        // it via max-width/max-height without reflowing each
        // frame. Apply once on the first successful fetch.
        const stage = canvas.parentElement;
        if (stage) {
          stage.style.aspectRatio = `${meta.width} / ${meta.height}`;
        }
        aspectApplied = true;
      }
      drawFrame(
        canvas,
        img,
        meta.objects,
        { width: meta.width, height: meta.height },
        {
          debugStatic: debugStaticEnabled,
          ...(showZonesEnabled && zones.length > 0 ? { zones } : {}),
        },
      );
    } catch {
      // Engine restart, no frame yet, etc. Keep polling.
    }
    await sleep(100);
  }
}

function once(el: HTMLImageElement, evt: "load" | "error"): Promise<void> {
  return new Promise((resolve) => {
    const onEvt = () => {
      el.removeEventListener(evt, onEvt);
      resolve();
    };
    el.addEventListener(evt, onEvt);
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
