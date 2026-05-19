import { api } from "../api/client.js";
import { subscribeSse } from "../api/sse.js";
import { h } from "../lib/el.js";
import { formatConfidence, formatLocalTime, formatTimeTooltip } from "../lib/format.js";
import type { AlertEvent, RuleConfig } from "../api/types.js";
import { openClipModal } from "./timeline.js";

/// Local cache of `rule_id -> name` so the ticker can show the
/// human-readable rule name (e.g. "After-hours vehicle in zone")
/// rather than the slug (`after_hours_vehicle`). Populated on mount
/// and refreshed lazily whenever an alert arrives for an id we
/// haven't seen yet — so adding/renaming a rule in another tab
/// surfaces here within one alert cycle without a polling loop.
const ruleNameCache = new Map<string, string>();
let ruleCachePrime: Promise<void> | null = null;

function primeRuleCache(): Promise<void> {
  if (ruleCachePrime) return ruleCachePrime;
  ruleCachePrime = (async () => {
    try {
      const rules = await api.rules.list();
      for (const r of rules) ruleNameCache.set(r.id, r.name);
    } catch {
      // Best-effort cache. We still render the rule_id slug
      // verbatim if the lookup fails — the alert is more
      // important than the prettified name.
    }
  })();
  return ruleCachePrime;
}

/// Resolve a rule_id to a display name, preferring (in order):
///   1. The `context.rule_name` the engine stamps onto every
///      AlertEvent (most authoritative — matches the rule version
///      that actually fired, even if it was renamed mid-session).
///   2. The cached `RuleConfig.name` from `/api/rules`.
///   3. The raw rule_id slug, as last-resort fallback.
function ruleDisplayName(ev: AlertEvent): string {
  const stamped = ev.context?.["rule_name"];
  if (typeof stamped === "string" && stamped.length > 0) return stamped;
  return ruleNameCache.get(ev.rule_id) ?? ev.rule_id;
}

export function mountAlertTicker(root: HTMLElement): void {
  const list = h("div", null);
  root.append(h("h3", null, "Live alerts"), list);
  void primeRuleCache();
  let count = 0;
  subscribeSse<AlertEvent>("/api/stream/events", (ev) => {
    // If this rule_id isn't in the cache yet (e.g. the rule was
    // added after the UI loaded), kick a refetch so the NEXT
    // alert for that id renders the pretty name. The current row
    // still shows the slug; better than blocking the prepend.
    if (!ruleNameCache.has(ev.rule_id) && !ev.context?.["rule_name"]) {
      ruleCachePrime = null;
      void primeRuleCache();
    }
    list.prepend(card(ev));
    count++;
    while (list.childElementCount > 50) {
      const last = list.lastElementChild;
      if (last) list.removeChild(last);
    }
    void count;
  });
}

function card(ev: AlertEvent): HTMLElement {
  // Play button looks up the supervisor-stamped clip_id on demand:
  // the SSE payload itself can't carry it because `link_event_to_clip`
  // runs AFTER the bus broadcast. Inline status text replaces the
  // button label on 404 so reviewers know whether the alert just
  // fired without an open recorder vs. a transient race.
  const status = h("span", { class: "alert-play-status muted" });
  const play = h(
    "button",
    {
      class: "alert-play-btn",
      title: "Play clip",
      type: "button",
      on: {
        click: async () => {
          play.disabled = true;
          status.textContent = "";
          try {
            const resp = await api.events.clip(ev.event_id);
            // Pin the overlay to THIS alert's track so a clip that
            // happens to carry multiple unrelated alerts (e.g. a
            // mislabeled-sign "person" alert + a real "car" alert)
            // doesn't draw every track's bbox on top of the car
            // playback. `track_id` is optional on `AlertEvent`
            // (legacy synthetic alerts can have NULL); fall back to
            // the clip-wide trigger set when missing.
            openClipModal(
              resp.clip_id,
              ev.track_id != null ? { focusTrackId: ev.track_id } : {},
            );
          } catch (err) {
            const msg = err instanceof Error ? err.message : String(err);
            status.textContent = msg.includes("404")
              ? "No clip yet"
              : "Lookup failed";
          } finally {
            play.disabled = false;
          }
        },
      },
    },
    "▶",
  );
  const ruleName = ruleDisplayName(ev);
  const conf = formatConfidence(ev.context?.["confidence"]);
  // Time element gets the raw UTC ISO in `title=` so cross-
  // referencing engine logs (which are UTC) is still one hover
  // away. `datetime` attribute set via `setAttribute` because
  // `h()` would otherwise assign a JS expando (`HTMLTimeElement`
  // exposes the prop as `dateTime` camelCase).
  const timeEl = h(
    "time",
    {
      class: "muted alert-time",
      title: formatTimeTooltip(ev.captured_at),
    },
    formatLocalTime(ev.captured_at, "time"),
  );
  timeEl.setAttribute("datetime", ev.captured_at);
  // Rule chip carries the id in `title=` for operators who want
  // the slug (e.g. when grepping config TOML).
  const ruleEl = h(
    "span",
    { class: "muted alert-rule", title: `rule_id: ${ev.rule_id}` },
    `· ${ruleName}`,
  );
  const metaBits: (string | HTMLElement)[] = [
    h("strong", null, ev.label),
    " ",
    h("span", { class: "muted" }, `· cam ${ev.camera_id}`),
    " ",
    ruleEl,
  ];
  if (conf) {
    metaBits.push(
      " ",
      h("span", { class: "muted alert-conf" }, `· ${conf} confidence`),
    );
  }
  return h(
    "div",
    { class: `alert severity-${ev.severity}` },
    h("div", { class: "alert-head" }, ...metaBits, play, status),
    timeEl,
  );
}
