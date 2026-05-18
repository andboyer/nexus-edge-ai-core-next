// M-Admin Phase 2 Step 2 — polygon zone editor.
//
// Opens a modal that overlays an editable polygon canvas on top of
// the camera's latest snapshot, and returns the new `ZoneConfig[]`
// on Save (or `null` on Cancel). The caller is responsible for
// PUT-ing the updated `CameraConfig` back to the engine.
//
// Coordinate model: polygon vertices are stored as normalized
// `[0..1, 0..1]` tuples in `ZoneConfig.polygon`, matching the
// engine's `point_in_normalized_polygon` contract. We convert to
// CSS pixels only when rendering / hit-testing in the canvas.
//
// Editing UX:
//   - "Add zone" → creates a new in-progress polygon. Click on the
//     canvas to drop vertices. Double-click (or "Close polygon"
//     button) to finalise once ≥3 vertices exist.
//   - "Edit" on a finalised zone → drag any vertex to move it,
//     shift-click a vertex to delete it (≥3 must remain).
//   - "Delete" removes a whole zone.
//   - Kind dropdown per zone: Inclusion (default, observational —
//     drives `motion.zone_state`), Exclusion (engine drops any
//     detection whose bbox centre falls inside), Dwell (reserved).
//
// Snapshot: pulled from `api.cameras.latestSnapshotUrl(id)`. If the
// image fails to load (camera disabled / no frames yet) we show a
// dimmed grey backdrop so the operator can still draft polygons —
// the geometry is normalized, so resolution drift between draft and
// live is harmless.

import { h, clear } from "../lib/el.js";
import { openDialog, dialogFooter, type DialogHandle } from "../lib/dialog.js";
import { Select } from "../lib/forms.js";
import { api } from "../api/client.js";
import type { CameraConfig, ZoneConfig } from "../api/types.js";

type ZoneKind = NonNullable<ZoneConfig["kind"]>;

interface EditorState {
  zones: ZoneConfig[];
  /// Index of the zone currently being drawn / edited. `null` means
  /// nothing is selected and clicks on the canvas do nothing.
  selectedIdx: number | null;
  /// True while the user is drafting a NEW polygon (clicks add
  /// vertices). False once the zone has been "closed" (≥3 verts).
  drafting: boolean;
  /// Index of the vertex currently being dragged (within the
  /// selected zone's polygon). `null` when no drag is in progress.
  dragVert: number | null;
}

const KIND_OPTIONS: ReadonlyArray<{ value: ZoneKind; label: string }> = [
  { value: "inclusion", label: "Inclusion (observe)" },
  { value: "exclusion", label: "Exclusion (drop detections)" },
  { value: "dwell", label: "Dwell (reserved)" },
];

const KIND_STROKE: Record<ZoneKind, string> = {
  inclusion: "#22c55e", // green
  exclusion: "#ef4444", // red
  dwell: "#f59e0b", // amber
};

const KIND_FILL: Record<ZoneKind, string> = {
  inclusion: "rgba(34, 197, 94, 0.18)",
  exclusion: "rgba(239, 68, 68, 0.22)",
  dwell: "rgba(245, 158, 11, 0.18)",
};

/// Open the polygon zone editor for `cam`. Resolves to the new
/// `ZoneConfig[]` if the user pressed Save, `null` on Cancel/ESC.
/// The caller decides whether to persist (we never call the engine
/// from here — keeps the editor reusable from any form context).
export function openZonesEditor(
  cam: CameraConfig,
  initial: ReadonlyArray<ZoneConfig>,
): Promise<ZoneConfig[] | null> {
  const state: EditorState = {
    zones: initial.map(cloneZone),
    selectedIdx: initial.length > 0 ? 0 : null,
    drafting: false,
    dragVert: null,
  };

  const root = h("div", { class: "zones-editor" });
  const canvasHost = h("div", { class: "zones-canvas-host" });
  const sidebar = h("div", { class: "zones-sidebar" });
  root.append(canvasHost, sidebar);

  // Snapshot <img> sized to its natural resolution; the canvas
  // overlays it 1:1 in CSS pixels. We size the canvas to the
  // rendered image box so hit-testing matches what the operator
  // sees regardless of source resolution.
  const img = h("img", {
    class: "zones-snapshot-img",
    alt: `Camera ${cam.id} snapshot`,
  });
  const canvas = h("canvas", { class: "zones-canvas-overlay" });
  const hint = h(
    "div",
    { class: "zones-canvas-hint" },
    "Loading snapshot…",
  );
  canvasHost.append(img, canvas, hint);

  // Bind snapshot load. On error fall back to a placeholder so the
  // operator can still draft (normalized coords mean a different
  // backing image is fine).
  img.onload = () => {
    hint.style.display = "none";
    resizeCanvas();
    redraw();
  };
  img.onerror = () => {
    hint.textContent =
      "No live snapshot yet — drafting on a placeholder. Zone coordinates are normalized so they'll line up once the camera is streaming.";
    hint.classList.add("zones-canvas-hint-error");
    canvasHost.classList.add("zones-canvas-no-snapshot");
    // Give the canvas an explicit 16:9 box so the operator has
    // something to click on.
    canvas.width = 960;
    canvas.height = 540;
    canvas.style.width = "100%";
    canvas.style.aspectRatio = "16 / 9";
    redraw();
  };
  // Cache-bust so the modal always pulls a fresh frame.
  img.src = api.cameras.latestSnapshotUrl(cam.id);

  function resizeCanvas(): void {
    // Match the canvas backing store to the displayed image size so
    // strokes stay crisp on HiDPI and hit-tests match cursor pixels.
    const rect = img.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(rect.width * dpr);
    canvas.height = Math.round(rect.height * dpr);
    canvas.style.width = `${rect.width}px`;
    canvas.style.height = `${rect.height}px`;
    const ctx = canvas.getContext("2d");
    if (ctx) ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  function redraw(): void {
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const w = canvas.clientWidth || canvas.width;
    const hPx = canvas.clientHeight || canvas.height;
    ctx.clearRect(0, 0, w, hPx);

    state.zones.forEach((z, i) => {
      const kind = (z.kind ?? "inclusion") as ZoneKind;
      const stroke = KIND_STROKE[kind];
      const fill = KIND_FILL[kind];
      const isSelected = state.selectedIdx === i;

      if (z.polygon.length === 0) return;

      ctx.beginPath();
      z.polygon.forEach(([nx, ny], vi) => {
        const x = nx * w;
        const y = ny * hPx;
        if (vi === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      });
      if (z.polygon.length >= 3 && !(isSelected && state.drafting)) {
        ctx.closePath();
        ctx.fillStyle = fill;
        ctx.fill();
      }
      ctx.lineWidth = isSelected ? 2.5 : 1.5;
      ctx.strokeStyle = stroke;
      ctx.stroke();

      // Vertex handles for the selected zone only — keeps the
      // overlay readable when many zones share the frame.
      if (isSelected) {
        z.polygon.forEach(([nx, ny]) => {
          const x = nx * w;
          const y = ny * hPx;
          ctx.beginPath();
          ctx.arc(x, y, 5, 0, Math.PI * 2);
          ctx.fillStyle = "#0b1220";
          ctx.fill();
          ctx.lineWidth = 2;
          ctx.strokeStyle = stroke;
          ctx.stroke();
        });
      }
    });
  }

  // ----- Canvas interaction --------------------------------------------------

  function clickToNormalized(ev: MouseEvent): [number, number] {
    const rect = canvas.getBoundingClientRect();
    const x = (ev.clientX - rect.left) / rect.width;
    const y = (ev.clientY - rect.top) / rect.height;
    return [clamp01(x), clamp01(y)];
  }

  function vertexHitTest(
    ev: MouseEvent,
    poly: ReadonlyArray<[number, number]>,
  ): number | null {
    const rect = canvas.getBoundingClientRect();
    const cx = ev.clientX - rect.left;
    const cy = ev.clientY - rect.top;
    const HIT_R = 10;
    for (let i = poly.length - 1; i >= 0; i--) {
      const v = poly[i]!;
      const vx = v[0] * rect.width;
      const vy = v[1] * rect.height;
      if (Math.hypot(cx - vx, cy - vy) <= HIT_R) return i;
    }
    return null;
  }

  canvas.addEventListener("mousedown", (ev) => {
    if (state.selectedIdx === null) return;
    const zone = state.zones[state.selectedIdx];
    if (!zone) return;

    // Shift-click on a vertex deletes it (when finalised + > 3 verts).
    if (ev.shiftKey && !state.drafting) {
      const vi = vertexHitTest(ev, zone.polygon);
      if (vi !== null && zone.polygon.length > 3) {
        zone.polygon.splice(vi, 1);
        rerenderSidebar();
        redraw();
        ev.preventDefault();
        return;
      }
    }

    // Start dragging an existing vertex.
    if (!state.drafting) {
      const vi = vertexHitTest(ev, zone.polygon);
      if (vi !== null) {
        state.dragVert = vi;
        ev.preventDefault();
        return;
      }
    }

    // Drafting → add a new vertex at the click position.
    if (state.drafting) {
      const [nx, ny] = clickToNormalized(ev);
      zone.polygon.push([nx, ny]);
      rerenderSidebar();
      redraw();
      ev.preventDefault();
    }
  });

  canvas.addEventListener("mousemove", (ev) => {
    if (state.dragVert === null || state.selectedIdx === null) return;
    const zone = state.zones[state.selectedIdx];
    if (!zone) return;
    const [nx, ny] = clickToNormalized(ev);
    zone.polygon[state.dragVert] = [nx, ny];
    redraw();
  });

  const endDrag = (): void => {
    if (state.dragVert !== null) {
      state.dragVert = null;
      rerenderSidebar();
    }
  };
  canvas.addEventListener("mouseup", endDrag);
  canvas.addEventListener("mouseleave", endDrag);

  canvas.addEventListener("dblclick", (ev) => {
    // Finalise the in-progress polygon on double-click (matches the
    // "Close polygon" button — operators expect both).
    if (
      state.drafting &&
      state.selectedIdx !== null &&
      (state.zones[state.selectedIdx]?.polygon.length ?? 0) >= 3
    ) {
      state.drafting = false;
      rerenderSidebar();
      redraw();
      ev.preventDefault();
    }
  });

  window.addEventListener("resize", onResize);
  function onResize(): void {
    if (img.complete && img.naturalWidth > 0) {
      resizeCanvas();
      redraw();
    }
  }

  // ----- Sidebar -------------------------------------------------------------

  function rerenderSidebar(): void {
    clear(sidebar);
    const header = h(
      "div",
      { class: "zones-sidebar-head" },
      h("h3", null, "Zones"),
      h(
        "button",
        {
          type: "button",
          class: "btn btn-primary btn-sm",
          on: { click: () => addZone() },
        },
        "+ Add zone",
      ),
    );
    sidebar.append(header);

    if (state.zones.length === 0) {
      sidebar.append(
        h(
          "p",
          { class: "zones-empty" },
          "No zones defined. Click + Add zone, then click on the snapshot to drop vertices. Double-click to close the polygon.",
        ),
      );
      return;
    }

    const list = h("ol", { class: "zones-list" });
    state.zones.forEach((z, i) => {
      const isSelected = state.selectedIdx === i;
      const item = h("li", {
        class: `zones-list-item${isSelected ? " is-selected" : ""}`,
      });

      const nameInput = h("input", {
        type: "text",
        class: "zones-name-input",
        value: z.name,
        placeholder: "Zone name",
        on: {
          input: (ev) => {
            z.name = (ev.currentTarget as HTMLInputElement).value;
          },
        },
      });

      const kindSelect = Select<ZoneKind>({
        label: "Kind",
        value: (z.kind ?? "inclusion") as ZoneKind,
        options: KIND_OPTIONS,
        onChange: (next) => {
          z.kind = next;
          redraw();
        },
      });

      const meta = h(
        "div",
        { class: "zones-list-meta" },
        h("span", { class: "zones-vert-count" }, `${z.polygon.length} verts`),
        state.drafting && isSelected
          ? h("span", { class: "zones-drafting-badge" }, "drafting")
          : null,
      );

      const actions = h("div", { class: "zones-list-actions" });

      if (state.drafting && isSelected) {
        actions.append(
          h(
            "button",
            {
              type: "button",
              class: "btn btn-sm",
              disabled: z.polygon.length < 3,
              on: { click: () => closePolygon() },
            },
            "Close polygon",
          ),
        );
      } else {
        actions.append(
          h(
            "button",
            {
              type: "button",
              class: "btn btn-sm",
              on: { click: () => selectZone(i) },
            },
            isSelected ? "Editing" : "Edit",
          ),
        );
      }
      actions.append(
        h(
          "button",
          {
            type: "button",
            class: "btn btn-sm btn-danger",
            on: { click: () => deleteZone(i) },
          },
          "Delete",
        ),
      );

      item.append(nameInput, kindSelect, meta, actions);
      list.append(item);
    });
    sidebar.append(list);

    sidebar.append(
      h(
        "p",
        { class: "zones-hints" },
        "Click on the snapshot to drop vertices · Drag a handle to move · Shift-click a handle to delete it",
      ),
    );
  }

  function addZone(): void {
    const id = newZoneId();
    state.zones.push({
      id,
      name: `Zone ${state.zones.length + 1}`,
      polygon: [],
      kind: "inclusion",
    });
    state.selectedIdx = state.zones.length - 1;
    state.drafting = true;
    rerenderSidebar();
    redraw();
  }

  function selectZone(i: number): void {
    state.selectedIdx = i;
    state.drafting = false;
    state.dragVert = null;
    rerenderSidebar();
    redraw();
  }

  function deleteZone(i: number): void {
    state.zones.splice(i, 1);
    if (state.selectedIdx !== null) {
      if (state.zones.length === 0) state.selectedIdx = null;
      else if (state.selectedIdx >= state.zones.length)
        state.selectedIdx = state.zones.length - 1;
      else if (state.selectedIdx === i) state.selectedIdx = null;
    }
    state.drafting = false;
    state.dragVert = null;
    rerenderSidebar();
    redraw();
  }

  function closePolygon(): void {
    if (state.selectedIdx === null) return;
    const zone = state.zones[state.selectedIdx];
    if (!zone || zone.polygon.length < 3) return;
    state.drafting = false;
    rerenderSidebar();
    redraw();
  }

  rerenderSidebar();

  // ----- Dialog plumbing -----------------------------------------------------

  let dlg: DialogHandle | null = null;
  let resolveFn!: (v: ZoneConfig[] | null) => void;
  const result = new Promise<ZoneConfig[] | null>((r) => {
    resolveFn = r;
  });

  const footer = dialogFooter({
    cancelLabel: "Cancel",
    confirmLabel: "Save zones",
    onCancel: () => {
      dlg?.close(false);
    },
    onConfirm: () => {
      // Drop any in-progress polygon with <3 vertices; finished
      // zones with degenerate polygons are also pruned silently.
      const cleaned = state.zones.filter((z) => z.polygon.length >= 3);
      resolveFn(cleaned);
      dlg?.close(true);
    },
  });

  dlg = openDialog({
    title: `Zones — camera ${cam.id} (${cam.name})`,
    body: root,
    footer,
    width: "min(1100px, 95vw)",
  });

  dlg.closed.then((saved) => {
    window.removeEventListener("resize", onResize);
    if (!saved) resolveFn(null);
  });

  return result;
}

// ----- helpers ---------------------------------------------------------------

function cloneZone(z: ZoneConfig): ZoneConfig {
  return {
    id: z.id,
    name: z.name,
    polygon: z.polygon.map(([x, y]) => [x, y] as [number, number]),
    ...(z.kind !== undefined ? { kind: z.kind } : {}),
  };
}

function clamp01(v: number): number {
  if (v < 0) return 0;
  if (v > 1) return 1;
  return v;
}

function newZoneId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  return `zone_${Date.now().toString(36)}_${Math.random()
    .toString(36)
    .slice(2, 8)}`;
}
