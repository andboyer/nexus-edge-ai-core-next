// M7 Step 6B — reusable weekly schedule editor.
//
// 7 days × 48 half-hour slots, click-and-drag paint, preset
// buttons (Always / Business hours / Nights & weekends / Off),
// timezone strip, and a "now" indicator that highlights the
// half-hour the local clock currently sits in.
//
// Owned state: the component holds a private `boolean[][]` mirror
// of the grid and emits `onChange` with a defensive copy after
// every mutation. Callers can pass a fresh `value` to reset.
//
// Wire contract (matches `nexus_types::DeliverySchedule`):
//   * grid.length === 7   — day 0 is Monday (chrono::Weekday::num_days_from_monday()).
//   * grid[d].length === 48 — slot = hour*2 + (minute >= 30 ? 1 : 0).
// The engine 400s anything that doesn't match in
// `DeliverySchedule::validate`; the editor enforces shape on
// construction so the form can never round-trip a bad grid.
//
// DOM gotcha (already burned us in viewer.ts on hash change): the
// `h()` `style` prop is typed `Partial<CSSStyleDeclaration>` but
// TS won't catch a string-typed style — pass plain objects only.

import { h } from "./el.js";

/// 7 outer × 48 inner. Day 0 = Monday. Slot 0 = 00:00–00:30 local.
export type ScheduleGrid = boolean[][];

const DAYS = 7;
const SLOTS_PER_DAY = 48;

/// Human-readable day labels in storage order. UI strings only;
/// the engine never sees these. Two-letter labels keep the column
/// header thin enough to leave room for the 48 slot cells.
const DAY_LABELS: ReadonlyArray<string> = [
  "Mon",
  "Tue",
  "Wed",
  "Thu",
  "Fri",
  "Sat",
  "Sun",
];

interface PaintState {
  /// What to paint into every cell touched by the current drag.
  /// Mirrors the inverse of the cell the drag started on so a
  /// click on a `true` cell turns the run off (and vice-versa).
  fillValue: boolean;
  /// Mouse-button state — pointer-move only paints while held.
  active: boolean;
}

export interface ScheduleEditorOpts {
  /// Initial grid. Pass `null` to start from "always on" (every
  /// slot true), matching the engine's `DeliverySchedule::always`
  /// preset. The editor copies the grid; callers stay free to
  /// pass aliased buffers.
  value: ScheduleGrid | null;
  /// IANA timezone for the strip label. Display-only — the editor
  /// does not parse or validate; the parent form's tz field is
  /// authoritative.
  timezone: string;
  /// Fired after every mutation (paint, preset, drag). Receives a
  /// defensive deep copy so the caller can stash it without
  /// worrying about future internal mutations.
  onChange: (next: ScheduleGrid) => void;
  /// Optional. Renders a read-only banner over the grid so the
  /// schedule still shows but cells don't paint on click. Useful
  /// for the per-rule editor when the operator picked "Inherit
  /// from global" — they should still see what global is, but
  /// not edit it through the rule form.
  readOnly?: boolean;
}

/// Build a fresh empty (all `false`) grid. Exposed so callers can
/// fabricate a preset shape without owning the constants.
export function emptySchedule(): ScheduleGrid {
  return Array.from({ length: DAYS }, () => new Array<boolean>(SLOTS_PER_DAY).fill(false));
}

/// Build a fresh full (all `true`) grid. Matches the engine's
/// `DeliverySchedule::always` factory.
export function alwaysSchedule(): ScheduleGrid {
  return Array.from({ length: DAYS }, () => new Array<boolean>(SLOTS_PER_DAY).fill(true));
}

/// Business hours: Monday–Friday, 08:00 to 18:00 (slot 16 to slot
/// 36, inclusive lower / exclusive upper). Weekend rows are all
/// false. Matches the M7 doc's "Business hours" preset.
export function businessHoursSchedule(): ScheduleGrid {
  const g = emptySchedule();
  for (let d = 0; d < 5; d++) {
    const row = g[d];
    if (!row) continue;
    for (let s = 16; s < 36; s++) row[s] = true;
  }
  return g;
}

/// Nights & weekends: every day, 00:00–08:00 (slots 0–15) and
/// 18:00–24:00 (slots 36–47); Saturday + Sunday all true. The
/// inverse of `businessHoursSchedule` when restricted to weekdays,
/// plus full weekend coverage.
export function nightsAndWeekendsSchedule(): ScheduleGrid {
  const g = emptySchedule();
  for (let d = 0; d < 5; d++) {
    const row = g[d];
    if (!row) continue;
    for (let s = 0; s < 16; s++) row[s] = true;
    for (let s = 36; s < 48; s++) row[s] = true;
  }
  for (let d = 5; d < 7; d++) {
    const row = g[d];
    if (!row) continue;
    row.fill(true);
  }
  return g;
}

/// Deep-clone a grid. The editor uses this on every emit so
/// callers can stash without worrying about future mutations.
export function cloneSchedule(g: ScheduleGrid): ScheduleGrid {
  return g.map((row) => row.slice());
}

/// Coerce an inbound grid (possibly from a deserialized server
/// payload) into the editor's invariants: 7 × 48, every cell a
/// strict boolean. Out-of-shape rows are padded/truncated; non-
/// boolean cells are coerced via `!!`. This is belt-and-suspenders
/// — the engine validates the same way on PUT — but it prevents a
/// stale snapshot crashing the editor render.
function normalize(g: ScheduleGrid | null): ScheduleGrid {
  if (!g) return alwaysSchedule();
  const out: ScheduleGrid = [];
  for (let d = 0; d < DAYS; d++) {
    const row = new Array<boolean>(SLOTS_PER_DAY).fill(false);
    const src = g[d];
    if (src) {
      for (let s = 0; s < SLOTS_PER_DAY; s++) row[s] = !!src[s];
    }
    out.push(row);
  }
  return out;
}

/// Render a 30-minute label for slot `s` (`0..47`). Half-hour
/// resolution → 24 of the 48 ticks need a printed label; the rest
/// stay blank so the column header doesn't smear.
function slotTickLabel(s: number): string {
  if (s % 4 !== 0) return ""; // every 2 hours
  const hour = Math.floor(s / 2);
  return hour.toString().padStart(2, "0");
}

/// Build the `<WeeklyScheduleEditor>` and return its root element.
/// The caller is responsible for inserting it into the DOM.
export function WeeklyScheduleEditor(opts: ScheduleEditorOpts): HTMLElement {
  const grid: ScheduleGrid = normalize(opts.value);
  const paint: PaintState = { fillValue: false, active: false };

  const root = h("div", { class: "schedule-editor" });

  // ---- Preset row -------------------------------------------------------
  const presets = h("div", { class: "schedule-presets" });
  const presetBtn = (label: string, build: () => ScheduleGrid, title: string): HTMLElement =>
    h(
      "button",
      {
        type: "button",
        class: "ghost",
        title,
        on: {
          click: () => {
            if (opts.readOnly) return;
            const next = build();
            for (let d = 0; d < DAYS; d++) {
              const dstRow = grid[d];
              const srcRow = next[d];
              if (dstRow && srcRow) {
                for (let s = 0; s < SLOTS_PER_DAY; s++) dstRow[s] = !!srcRow[s];
              }
            }
            repaint();
            opts.onChange(cloneSchedule(grid));
          },
        },
      },
      label,
    );
  presets.append(
    h("span", { class: "schedule-presets-label" }, "Preset:"),
    presetBtn("Always", alwaysSchedule, "Every half-hour slot on."),
    presetBtn(
      "Business hours",
      businessHoursSchedule,
      "Monday through Friday, 08:00 to 18:00 local time.",
    ),
    presetBtn(
      "Nights & weekends",
      nightsAndWeekendsSchedule,
      "Weekdays 00:00–08:00 + 18:00–24:00, plus all of Saturday and Sunday.",
    ),
    presetBtn("Off", emptySchedule, "Every half-hour slot off."),
  );
  if (opts.readOnly) {
    for (const b of Array.from(presets.querySelectorAll("button"))) {
      b.setAttribute("disabled", "true");
    }
  }
  root.append(presets);

  // ---- Timezone strip + "now" indicator --------------------------------
  const tzStrip = h(
    "div",
    { class: "schedule-tz-strip" },
    h("span", { class: "muted" }, "Timezone "),
    h("strong", null, opts.timezone),
    h(
      "span",
      { class: "muted" },
      " · Day 0 = Monday; slot ticks mark even hours (00, 02, … 22).",
    ),
  );
  root.append(tzStrip);

  // ---- Grid table -------------------------------------------------------
  // Plain <table> so the day labels line up with the rows
  // semantically and screen readers can navigate the cells; the
  // half-hour cells are <td>s with role="button" for keyboard
  // focus.
  const table = h("table", { class: "schedule-grid" });

  // Tick header row.
  const thead = h("thead", null);
  const tickRow = h("tr", null);
  tickRow.append(h("th", { class: "schedule-day-corner" }, ""));
  for (let s = 0; s < SLOTS_PER_DAY; s++) {
    const lbl = slotTickLabel(s);
    tickRow.append(
      h(
        "th",
        {
          class:
            "schedule-tick" +
            (s % 4 === 0 ? " schedule-tick-major" : "") +
            (s % 2 === 0 ? " schedule-tick-hour" : ""),
          title: `Slot ${s} (${formatSlotRange(s)})`,
        },
        lbl,
      ),
    );
  }
  thead.append(tickRow);
  table.append(thead);

  // Body rows: one per day, 48 cells each.
  const tbody = h("tbody", null);
  const cellRefs: HTMLTableCellElement[][] = [];
  for (let d = 0; d < DAYS; d++) {
    const row = h("tr", null);
    row.append(h("th", { class: "schedule-day-label" }, DAY_LABELS[d] ?? `${d}`));
    const cells: HTMLTableCellElement[] = [];
    for (let s = 0; s < SLOTS_PER_DAY; s++) {
      const cell = h("td", {
        class: "schedule-cell",
        title: `${DAY_LABELS[d] ?? d} ${formatSlotRange(s)}`,
      }) as HTMLTableCellElement;
      // role + tabindex so keyboard users can tab to a cell and
      // toggle it with Enter/Space. Pointer interaction is the
      // primary path; this just keeps the editor accessible.
      cell.setAttribute("role", "button");
      cell.setAttribute("tabindex", "-1");
      cell.dataset["d"] = String(d);
      cell.dataset["s"] = String(s);
      cell.addEventListener("pointerdown", (ev) => {
        if (opts.readOnly) return;
        ev.preventDefault();
        const current = !!grid[d]?.[s];
        paint.fillValue = !current;
        paint.active = true;
        setCell(d, s, paint.fillValue);
        try {
          (ev.target as HTMLElement).setPointerCapture?.(ev.pointerId);
        } catch {
          // Older browsers — pointermove on window still works.
        }
      });
      cell.addEventListener("pointerenter", () => {
        if (!paint.active || opts.readOnly) return;
        setCell(d, s, paint.fillValue);
      });
      cell.addEventListener("keydown", (ev) => {
        if (opts.readOnly) return;
        if (ev.key === "Enter" || ev.key === " ") {
          ev.preventDefault();
          setCell(d, s, !grid[d]?.[s]);
          opts.onChange(cloneSchedule(grid));
        }
      });
      row.append(cell);
      cells.push(cell);
    }
    tbody.append(row);
    cellRefs.push(cells);
  }
  table.append(tbody);
  root.append(table);

  // Global pointerup so a drag that exits the table still settles
  // (otherwise a release outside the grid would leave `active`
  // stuck `true` and the next move-over would repaint silently).
  const endDrag = (): void => {
    if (!paint.active) return;
    paint.active = false;
    opts.onChange(cloneSchedule(grid));
  };
  window.addEventListener("pointerup", endDrag);
  // Detach on remove. We don't have a lifecycle hook, but
  // `MutationObserver` on the root's parent is the smallest
  // primitive that survives ad-hoc tab switching (Cameras → some
  // other tab) without leaking the listener.
  const obs = new MutationObserver(() => {
    if (!root.isConnected) {
      window.removeEventListener("pointerup", endDrag);
      obs.disconnect();
    }
  });
  // Watch the document body so removal from any subtree triggers
  // the cleanup check; cheap because we only react to subtree
  // mutations and bail early.
  obs.observe(document.body, { childList: true, subtree: true });

  // ---- Initial paint + "now" highlight ---------------------------------
  repaint();
  highlightNow();
  // Tick the "now" cell every minute so the indicator advances
  // without a hard page refresh. The timer is cheap and gets
  // collected when the root is removed (`isConnected` guard).
  const nowTimer = window.setInterval(() => {
    if (!root.isConnected) {
      window.clearInterval(nowTimer);
      return;
    }
    highlightNow();
  }, 30_000);

  function setCell(d: number, s: number, value: boolean): void {
    const row = grid[d];
    if (!row) return;
    if (row[s] === value) return;
    row[s] = value;
    const cell = cellRefs[d]?.[s];
    if (cell) cell.classList.toggle("schedule-cell-on", value);
  }

  function repaint(): void {
    for (let d = 0; d < DAYS; d++) {
      const cells = cellRefs[d];
      const row = grid[d];
      if (!cells || !row) continue;
      for (let s = 0; s < SLOTS_PER_DAY; s++) {
        cells[s]?.classList.toggle("schedule-cell-on", !!row[s]);
      }
    }
  }

  /// Highlight the half-hour slot the operator's clock currently
  /// sits in. Uses the BROWSER's local timezone — not the schedule
  /// timezone — because the indicator is meant as a "where am I
  /// right now" hint, not a cascade preview. The schedule timezone
  /// is shown in the strip above for that.
  function highlightNow(): void {
    for (const row of cellRefs) {
      for (const c of row) c.classList.remove("schedule-cell-now");
    }
    const now = new Date();
    // JS Date.getDay() is 0 = Sunday, 1 = Monday … 6 = Saturday;
    // our grid uses 0 = Monday. Re-key with mod-7.
    const dayMon = (now.getDay() + 6) % 7;
    const slot = now.getHours() * 2 + (now.getMinutes() >= 30 ? 1 : 0);
    const cell = cellRefs[dayMon]?.[slot];
    if (cell) cell.classList.add("schedule-cell-now");
  }

  return root;
}

/// Format a half-hour slot as a `HH:MM–HH:MM` range. Used in the
/// title/tooltip for each cell.
function formatSlotRange(s: number): string {
  const startH = Math.floor(s / 2);
  const startM = (s % 2) * 30;
  const endTotal = s * 30 + 30;
  const endH = Math.floor(endTotal / 60) % 24;
  const endM = endTotal % 60;
  const fmt = (h: number, m: number): string =>
    `${h.toString().padStart(2, "0")}:${m.toString().padStart(2, "0")}`;
  return `${fmt(startH, startM)}–${fmt(endH, endM)}`;
}
