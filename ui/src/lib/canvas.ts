import type { TrackedObject, ZoneConfig } from "../api/types.js";

const PALETTE = [
  "#38e1ff",
  "#ffd166",
  "#ef476f",
  "#06d6a0",
  "#a78bfa",
  "#ff9f1c",
];

/// Muted slate-grey used for objects the static-object filter has
/// promoted (parked vehicles, etc.). Picked to contrast against the
/// active-track palette above without disappearing on typical street
/// footage (which is mostly grey itself). Mirrors the supervisor
/// contract in `crates/nexus-pipeline/src/supervisor.rs` — rules and
/// the motion lifecycle no longer see these objects, but the live
/// viewer still draws them so operators can see WHY a parked car
/// stopped firing alerts.
const STATIC_COLOR = "#94a3b8";

/// Attribute key the supervisor stamps onto suppressed tracks. Kept
/// in sync with `STATIC_ATTRIBUTE_KEY` in
/// `crates/nexus-tracker/src/static_object.rs`.
const STATIC_ATTR_KEY = "tracker.is_static";

/// Diagnostic attribute keys stamped on every vehicle-labelled track
/// by the static-object filter. Surfaced by the live viewer's
/// "static debug" toggle so an operator can see the exact EMA value
/// and dwell counters the FSM is using to decide whether to promote.
/// Kept in sync with the same-named consts in
/// `crates/nexus-tracker/src/static_object.rs`.
const EMA_ATTR_KEY = "tracker.movement_ema";
const STATIC_FRAMES_ATTR_KEY = "tracker.static_frames";
const MOVING_FRAMES_ATTR_KEY = "tracker.moving_consecutive_frames";

function colorFor(track_id: number): string {
  const i = Math.abs(Math.floor(track_id)) % PALETTE.length;
  return PALETTE[i] ?? "#38e1ff";
}

function isObjectStatic(o: TrackedObject): boolean {
  return o.attributes?.[STATIC_ATTR_KEY] === true;
}

function numAttr(o: TrackedObject, key: string): number | null {
  const v = o.attributes?.[key];
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

/// Optional rendering knobs. `debugStatic` adds a second line under
/// the label pill on every vehicle-tagged track, displaying the
/// static-object FSM's internal state (`ema`, `static_frames`,
/// `moving_consecutive_frames`). Drawn iff the diagnostic
/// attributes are present on the object — old clip metadata
/// without them just shows nothing extra.
///
/// `zones`, when non-empty, draws each polygon's outline + a
/// translucent fill on top of the image (but UNDER the bbox
/// overlays so detections stay legible). Color follows the zone
/// `kind`: inclusion = green, exclusion = red, dwell = amber. A
/// small label pill is drawn at the polygon's centroid so the
/// operator can match the overlay to the zone in the camera form.
export interface DrawFrameOpts {
  debugStatic?: boolean;
  zones?: ReadonlyArray<ZoneConfig>;
}

/// Resolve a zone color from its kind. Defaults to inclusion since
/// the engine treats a missing `kind` as inclusion (see
/// `nexus_config::ZoneConfig`).
function zoneColor(kind: ZoneConfig["kind"]): string {
  switch (kind) {
    case "exclusion":
      return "#ef4444";
    case "dwell":
      return "#fbbf24";
    case "inclusion":
    case undefined:
    default:
      return "#10b981";
  }
}

/** Draw a video frame from an `<img>` plus tracked-object overlays. */
export function drawFrame(
  canvas: HTMLCanvasElement,
  img: HTMLImageElement,
  objects: TrackedObject[],
  meta: { width: number; height: number },
  opts: DrawFrameOpts = {},
): void {
  // Fit the natural-image canvas to the displayed canvas size.
  const ratio = window.devicePixelRatio || 1;
  const cssW = canvas.clientWidth;
  const cssH = canvas.clientHeight;
  canvas.width = Math.round(cssW * ratio);
  canvas.height = Math.round(cssH * ratio);

  const ctx = canvas.getContext("2d");
  if (!ctx) return;

  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  ctx.clearRect(0, 0, cssW, cssH);

  if (img.complete && img.naturalWidth > 0) {
    // Letterbox.
    const sw = img.naturalWidth;
    const sh = img.naturalHeight;
    const scale = Math.min(cssW / sw, cssH / sh);
    const dw = sw * scale;
    const dh = sh * scale;
    const dx = (cssW - dw) / 2;
    const dy = (cssH - dh) / 2;
    ctx.drawImage(img, dx, dy, dw, dh);

    // Zone overlays — drawn before the bbox layer so detections
    // remain on top and the operator can see which polygon a track
    // is sitting in. Polygons are stored in normalized [0,1] coords
    // so we just multiply by the letterboxed image rect.
    if (opts.zones && opts.zones.length > 0) {
      const prevAlpha = ctx.globalAlpha;
      const prevLine = ctx.lineWidth;
      for (const z of opts.zones) {
        if (z.polygon.length < 3) continue;
        const color = zoneColor(z.kind);
        ctx.strokeStyle = color;
        ctx.fillStyle = color;
        ctx.lineWidth = 1.5;
        if (z.kind === "exclusion") ctx.setLineDash([5, 4]);
        else ctx.setLineDash([]);
        ctx.beginPath();
        for (let i = 0; i < z.polygon.length; i++) {
          const pt = z.polygon[i]!;
          const ax = dx + pt[0] * dw;
          const ay = dy + pt[1] * dh;
          if (i === 0) ctx.moveTo(ax, ay);
          else ctx.lineTo(ax, ay);
        }
        ctx.closePath();
        ctx.globalAlpha = 0.14;
        ctx.fill();
        ctx.globalAlpha = 0.9;
        ctx.stroke();

        // Label pill at polygon centroid. Centroid (not first
        // vertex) so the label stays anchored to the shape even
        // when an operator drags vertices around.
        if (z.name) {
          let cx = 0;
          let cy = 0;
          for (const pt of z.polygon) {
            cx += pt[0];
            cy += pt[1];
          }
          cx /= z.polygon.length;
          cy /= z.polygon.length;
          const lx = dx + cx * dw;
          const ly = dy + cy * dh;
          const labelW = ctx.measureText(z.name).width + 6;
          const labelH = 14;
          ctx.globalAlpha = 0.95;
          ctx.fillStyle = color;
          ctx.fillRect(lx - labelW / 2, ly - labelH / 2, labelW, labelH);
          ctx.fillStyle = "#0b0d10";
          ctx.globalAlpha = 1;
          ctx.fillText(z.name, lx - labelW / 2 + 3, ly + 4);
        }
      }
      ctx.setLineDash([]);
      ctx.globalAlpha = prevAlpha;
      ctx.lineWidth = prevLine;
    }

    // Overlay tracked objects in the same scaled coords.
    ctx.font = "12px monospace";
    for (const o of objects) {
      const x = dx + o.bbox.x1 * scale;
      const y = dy + o.bbox.y1 * scale;
      const w = (o.bbox.x2 - o.bbox.x1) * scale;
      const h = (o.bbox.y2 - o.bbox.y1) * scale;

      // Static objects (parked vehicles etc.) render in muted
      // slate with a dashed stroke and a "(static)" tag so the
      // operator can immediately tell them apart from live
      // tracks. Rules + motion events are already suppressed
      // for these tracks server-side, so the muted style is
      // also a visual "you won't get an alert on this one"
      // affordance.
      const isStatic = isObjectStatic(o);
      const c = isStatic ? STATIC_COLOR : colorFor(o.track_id);

      ctx.strokeStyle = c;
      ctx.fillStyle = c;
      ctx.lineWidth = isStatic ? 1.5 : 2;
      if (isStatic) {
        ctx.setLineDash([6, 4]);
        ctx.globalAlpha = 0.7;
      } else {
        ctx.setLineDash([]);
        ctx.globalAlpha = 1;
      }
      ctx.strokeRect(x, y, w, h);
      // Reset dash/alpha for the label pill — the pill itself
      // should be fully opaque so the text stays legible.
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;

      const tag = isStatic
        ? `#${o.track_id} ${o.label} (static)`
        : `#${o.track_id} ${o.label} ${(o.confidence * 100).toFixed(0)}%`;
      const padding = 3;
      const pillH = 16;
      const tw = ctx.measureText(tag).width + padding * 2;

      // ---- pill placement, clamped to the visible image rect ----
      //
      // Default position is just above the bbox left edge. When the
      // bbox hugs the right/left/top of the viewer the naive
      // placement (`x`, `y - pillH`) walks off the canvas and the
      // text becomes unreadable. Clamp horizontally into
      // `[dx, dx + dw - tw]`, and if there's no room above the box,
      // flip the pill INSIDE the box at its top edge instead. We
      // also need to track whether we flipped, because the debug
      // pill stacks relative to this one.
      let pillX = x;
      if (pillX + tw > dx + dw) pillX = dx + dw - tw;
      if (pillX < dx) pillX = dx;
      let pillY = y - pillH;
      let pillTextY = y - 4;
      const mainFlippedInside = pillY < dy;
      if (mainFlippedInside) {
        pillY = y;
        pillTextY = y + 12;
      }
      ctx.fillRect(pillX, pillY, tw, pillH);
      ctx.fillStyle = "#0b0d10";
      ctx.fillText(tag, pillX + padding, pillTextY);

      // Static-filter diagnostic line. Drawn iff the operator has
      // toggled debug on AND the FSM stamped the diagnostic attrs
      // (i.e. the camera is in parking-lot mode AND the label is a
      // vehicle). The pill stacks immediately above the main pill
      // when there's room; otherwise it stacks INSIDE the box just
      // below the (already-flipped) main pill so neither escapes
      // the viewer.
      if (opts.debugStatic) {
        const ema = numAttr(o, EMA_ATTR_KEY);
        const sf = numAttr(o, STATIC_FRAMES_ATTR_KEY);
        const mf = numAttr(o, MOVING_FRAMES_ATTR_KEY);
        if (ema !== null && sf !== null && mf !== null) {
          const dbg = `ema=${ema.toFixed(1)} sf=${sf} mv=${mf}`;
          const dbgW = ctx.measureText(dbg).width + padding * 2;
          let dbgX = x;
          if (dbgX + dbgW > dx + dw) dbgX = dx + dw - dbgW;
          if (dbgX < dx) dbgX = dx;
          let dbgY: number;
          let dbgTextY: number;
          if (mainFlippedInside) {
            // Main is inside-top; stack debug just below it (also
            // inside the box).
            dbgY = pillY + pillH;
            dbgTextY = dbgY + 12;
          } else {
            // Main is above the box; try stacking debug above main.
            dbgY = pillY - pillH;
            dbgTextY = pillY - 4;
            if (dbgY < dy) {
              // No room above main either — flip debug INSIDE the
              // box at the top edge.
              dbgY = y;
              dbgTextY = y + 12;
            }
          }
          ctx.fillStyle = "#1f2937";
          ctx.fillRect(dbgX, dbgY, dbgW, pillH);
          ctx.fillStyle = "#e5e7eb";
          ctx.fillText(dbg, dbgX + padding, dbgTextY);
        }
      }
    }
  }
  // Suppress meta-unused warning while keeping the API stable.
  void meta;
}
