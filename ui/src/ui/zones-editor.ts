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
//     canvas to drop vertices. Click the FIRST vertex (or double-
//     click anywhere) to finalise once ≥3 vertices exist; the
//     "Close polygon" button is still there as a fallback.
//   - Gear icon on a finalised zone → drag any vertex to move it,
//     click on an edge to insert a new vertex at that point,
//     shift-click a vertex to delete it (≥3 must remain).
//   - Trash icon removes a whole zone.
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
import { iconButton, icon } from "../lib/icons.js";
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

      // Zone-name label at the polygon centroid. Skipped while
      // drafting (the polygon is open + likely degenerate) and
      // when the name is empty.
      if (
        !(isSelected && state.drafting) &&
        z.polygon.length >= 3 &&
        z.name.trim() !== ""
      ) {
        let cx = 0;
        let cy = 0;
        for (const [nx, ny] of z.polygon) {
          cx += nx * w;
          cy += ny * hPx;
        }
        cx /= z.polygon.length;
        cy /= z.polygon.length;
        ctx.font =
          "600 13px system-ui, -apple-system, Segoe UI, Roboto, sans-serif";
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        // Dark halo so the label stays readable on bright frames.
        ctx.lineWidth = 4;
        ctx.strokeStyle = "rgba(0, 0, 0, 0.75)";
        ctx.strokeText(z.name, cx, cy);
        ctx.fillStyle = "#ffffff";
        ctx.fillText(z.name, cx, cy);
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

  /// Hit-test a click against the polygon's *edges* (line segments
  /// between consecutive vertices, including the closing segment
  /// from last → first). Returns the index AFTER which a new vertex
  /// should be inserted (so `poly[result]` and `poly[result+1]` are
  /// the two endpoints of the matched edge — modulo wraparound),
  /// or `null` if no edge is within `HIT_R` CSS pixels of the click.
  ///
  /// This is the "click a line to add a vertex" UX from
  /// `nexus-admin/static/js/zone-editor.js`. Without it, the only
  /// way to insert a vertex is to delete the polygon and redraw
  /// from scratch — which is bad UX for fine-tuning a zone you've
  /// already roughed in.
  function edgeHitTest(
    ev: MouseEvent,
    poly: ReadonlyArray<[number, number]>,
  ): number | null {
    if (poly.length < 2) return null;
    const rect = canvas.getBoundingClientRect();
    const cx = ev.clientX - rect.left;
    const cy = ev.clientY - rect.top;
    const HIT_R = 6;
    let bestIdx: number | null = null;
    let bestDist = HIT_R;
    // Iterate edges; for a finalised polygon the last edge closes
    // back to vertex 0.
    const edgeCount = poly.length; // closing edge included
    for (let i = 0; i < edgeCount; i++) {
      const a = poly[i]!;
      const b = poly[(i + 1) % poly.length]!;
      const ax = a[0] * rect.width;
      const ay = a[1] * rect.height;
      const bx = b[0] * rect.width;
      const by = b[1] * rect.height;
      const d = pointToSegmentDistance(cx, cy, ax, ay, bx, by);
      if (d < bestDist) {
        bestDist = d;
        bestIdx = i;
      }
    }
    return bestIdx;
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
      // No vertex hit — try the edge: a click near a polygon edge
      // inserts a new vertex at the click point. Lets operators
      // refine an existing shape without restarting.
      const ei = edgeHitTest(ev, zone.polygon);
      if (ei !== null) {
        const [nx, ny] = clickToNormalized(ev);
        zone.polygon.splice(ei + 1, 0, [nx, ny]);
        // Drag-state the freshly-inserted vertex so the operator
        // can keep refining without releasing the mouse.
        state.dragVert = ei + 1;
        rerenderSidebar();
        redraw();
        ev.preventDefault();
        return;
      }
    }

    // Drafting → add a new vertex at the click position. If the
    // click is on (or very near) the FIRST vertex and we already
    // have ≥3 verts, auto-close the polygon instead of adding a
    // duplicate point. Matches operator expectation from every
    // other polygon editor (PowerPoint, Inkscape, Google Maps).
    if (state.drafting) {
      if (zone.polygon.length >= 3) {
        const firstHit = vertexHitTest(ev, zone.polygon.slice(0, 1));
        if (firstHit !== null) {
          state.drafting = false;
          rerenderSidebar();
          redraw();
          ev.preventDefault();
          return;
        }
      }
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
    const addBtn = h(
      "button",
      {
        type: "button",
        class: "primary btn-with-icon",
        on: { click: () => addZone() },
      },
      icon("plus"),
      "Add zone",
    );
    const header = h(
      "div",
      { class: "zones-sidebar-head" },
      h("h3", null, "Zones"),
      addBtn,
    );
    sidebar.append(header);

    if (state.zones.length === 0) {
      sidebar.append(
        h(
          "p",
          { class: "zones-empty" },
          "No zones defined. Click + Add zone, then click on the snapshot to drop vertices. Click the first vertex (or double-click) to close the polygon.",
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
              class: "ghost",
              disabled: z.polygon.length < 3,
              on: { click: () => closePolygon() },
            },
            "Close polygon",
          ),
        );
      } else {
        actions.append(
          iconButton("gear", {
            title: isSelected ? "Currently editing" : `Edit ${z.name || "zone"}`,
            disabled: isSelected,
            onClick: () => selectZone(i),
          }),
        );
      }
      actions.append(
        iconButton("trash", {
          title: `Delete ${z.name || "zone"}`,
          onClick: () => deleteZone(i),
        }),
      );

      item.append(nameInput, kindSelect, meta, actions);
      list.append(item);
    });
    sidebar.append(list);

    sidebar.append(
      h(
        "p",
        { class: "zones-hints" },
        "Click on the snapshot to drop vertices · Click the first vertex (or double-click) to close · Click an edge to insert a new vertex · Drag a handle to move · Shift-click a handle to delete it",
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

/// Perpendicular distance from point `(px, py)` to the line
/// segment `(ax, ay) → (bx, by)` in CSS pixels. Used by the
/// edge-hit test that lets operators click on a polygon edge to
/// insert a new vertex. Uses the classic projection formula and
/// clamps the parameter `t` to [0,1] so endpoint-near clicks
/// return the endpoint distance (not the infinite-line distance).
function pointToSegmentDistance(
  px: number,
  py: number,
  ax: number,
  ay: number,
  bx: number,
  by: number,
): number {
  const dx = bx - ax;
  const dy = by - ay;
  const lenSq = dx * dx + dy * dy;
  if (lenSq === 0) return Math.hypot(px - ax, py - ay);
  let t = ((px - ax) * dx + (py - ay) * dy) / lenSq;
  if (t < 0) t = 0;
  else if (t > 1) t = 1;
  const cx = ax + t * dx;
  const cy = ay + t * dy;
  return Math.hypot(px - cx, py - cy);
}

function newZoneId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  return `zone_${Date.now().toString(36)}_${Math.random()
    .toString(36)
    .slice(2, 8)}`;
}
