// Small display-side formatters shared between the live alert
// ticker and the recent-events table. Kept tiny on purpose — no
// dayjs/luxon. `Intl.DateTimeFormat` covers everything we need
// and respects the browser's locale + timezone automatically.

/// Render an engine-supplied RFC3339/ISO-8601 timestamp (always
/// emitted in UTC by the Rust side via `Utc::now().to_rfc3339()`)
/// in the user's local timezone. Falls back to the raw string on
/// parse failure so we never strand the operator with "Invalid
/// Date" if the engine ever emits something unusual.
///
/// `style` controls density:
///   - "time": HH:MM:SS local — used by the live ticker where the
///     date is implicit (the row just arrived).
///   - "datetime": YYYY-MM-DD HH:MM:SS local — used by the recent
///     events table where rows may span days.
export function formatLocalTime(
  iso: string,
  style: "time" | "datetime" = "datetime",
): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  if (style === "time") {
    return d.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/// Full ISO timestamp formatted for a `title=` tooltip — gives the
/// reviewer the raw UTC value (useful for cross-referencing logs)
/// alongside the human local-time display. Detects the browser's
/// IANA zone via `Intl.DateTimeFormat().resolvedOptions().timeZone`
/// so the tooltip annotates which zone we converted into.
export function formatTimeTooltip(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone ?? "local";
  return `${iso}\n(displayed in ${tz})`;
}

/// Format a 0.0–1.0 confidence value as an integer percentage.
/// Returns `null` when the input isn't a finite number — callers
/// should treat that as "field not present" rather than rendering
/// "NaN%". The engine emits `f32` originally but the JSON round-
/// trip lands here as `number | undefined`.
export function formatConfidence(v: unknown): string | null {
  if (typeof v !== "number" || !Number.isFinite(v)) return null;
  return `${Math.round(v * 100)}%`;
}
