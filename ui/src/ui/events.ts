import { api } from "../api/client.js";
import { clear, h } from "../lib/el.js";
import { openDialog } from "../lib/dialog.js";
import { formatConfidence, formatLocalTime, formatTimeTooltip } from "../lib/format.js";
import type {
  AlertEvent,
  EventId,
  OutboxRow,
  OutboxStatus,
  RuleConfig,
  SuppressionReason,
} from "../api/types.js";

export async function renderEvents(root: HTMLElement): Promise<void> {
  clear(root);
  // Load events + the rules catalog in parallel. The rules list is
  // used as a `rule_id -> name` fallback for any older rows whose
  // engine version didn't stamp `context.rule_name`. Catalog errors
  // are non-fatal — we just lose the prettified name on those rows.
  const [list, rules] = await Promise.all([
    api.events.recent(200),
    api.rules.list().catch((): RuleConfig[] => []),
  ]);
  const ruleNames = new Map<string, string>();
  for (const r of rules) ruleNames.set(r.id, r.name);

  root.append(h("h2", null, "Recent events"));
  if (list.length === 0) {
    root.append(h("p", { class: "muted" }, "No events yet."));
    return;
  }
  const tbl = h(
    "table",
    null,
    h(
      "thead",
      null,
      h(
        "tr",
        null,
        h("th", null, "When"),
        h("th", null, "Camera"),
        h("th", null, "Rule"),
        h("th", null, "Label"),
        h("th", null, "Confidence"),
        h("th", null, "Severity"),
        h("th", null, "Trace"),
        h("th", null, "Delivery"),
      ),
    ),
    h("tbody", null, ...list.map((e) => row(e, ruleNames))),
  );
  root.append(tbl);
}

function row(e: AlertEvent, ruleNames: ReadonlyMap<string, string>): HTMLElement {
  // Rule display name precedence: engine-stamped `context.rule_name`
  // (matches the rule version that fired) → cached catalog name →
  // raw slug. See alert-ticker for the same lookup pattern.
  const stamped = e.context?.["rule_name"];
  const ruleName =
    typeof stamped === "string" && stamped.length > 0
      ? stamped
      : ruleNames.get(e.rule_id) ?? e.rule_id;
  const conf = formatConfidence(e.context?.["confidence"]);
  const whenCell = h(
    "td",
    { title: formatTimeTooltip(e.captured_at) },
    formatLocalTime(e.captured_at, "datetime"),
  );
  const ruleCell = h(
    "td",
    { title: `rule_id: ${e.rule_id}` },
    ruleName,
  );

  // M7 Step 6D — per-event delivery dialog opener. We do NOT
  // eager-fetch the per-event outbox rows for every event in the
  // table (would be N=200 HTTP calls); the row stays cheap and
  // the dialog loads on demand when the operator actually wants
  // to see how a specific alert routed. The cell still reads as
  // "View" so the affordance is obvious without a tooltip.
  const deliveryBtn = h(
    "button",
    {
      type: "button",
      class: "ghost delivery-open-btn",
      title:
        "Show per-sink delivery attempts for this event (GET /v1/events/:id/delivery).",
      on: {
        click: () => openDeliveryDialog(e.event_id),
      },
    },
    "View",
  );

  return h(
    "tr",
    null,
    whenCell,
    h("td", null, String(e.camera_id)),
    ruleCell,
    h("td", null, e.label),
    conf
      ? h("td", null, conf)
      : h("td", { class: "muted" }, "—"),
    h("td", null, e.severity),
    h("td", null, h("code", null, e.trace_id)),
    h("td", null, deliveryBtn),
  );
}

// ---------------------------------------------------------------------------
// M7 Step 6D — per-event delivery dialog.
//
// Renders the badge stack the M7 doc spec'd ("[webhook:primary ✓ sent]
// [sureview:siteX ⚠ 2 attempts] [webhook:secondary ⊘ off-schedule
// (global)]") plus the underlying detail table. The dialog is fully
// self-contained: it opens immediately with a "Loading…" placeholder
// and swaps the body once `api.delivery.listForEvent` resolves.
// ---------------------------------------------------------------------------

function openDeliveryDialog(eventId: EventId): void {
  const placeholder = h(
    "p",
    { class: "muted" },
    "Loading per-sink delivery attempts…",
  );
  const handle = openDialog({
    title: `Delivery — event ${eventId}`,
    body: placeholder,
    width: "640px",
  });

  api.delivery
    .listForEvent(eventId)
    .then((rows) => {
      const fresh = renderDeliveryBody(rows);
      handle.body.replaceChildren(fresh);
    })
    .catch((err) => {
      const msg = String((err as Error).message ?? err);
      const fresh = h(
        "p",
        { class: "muted" },
        `Failed to load delivery rows: ${msg}`,
      );
      handle.body.replaceChildren(fresh);
    });
}

function renderDeliveryBody(rows: ReadonlyArray<OutboxRow>): HTMLElement {
  if (rows.length === 0) {
    return h(
      "div",
      null,
      h(
        "p",
        { class: "muted" },
        "No outbox rows for this event — most likely no sinks are configured in cfg.sinks. Configure at least one sink to see per-event delivery attempts here.",
      ),
    );
  }

  // Badge stack on top — at-a-glance "who got what".
  const badgeStack = h("div", { class: "delivery-badge-stack" });
  for (const r of rows) badgeStack.append(deliveryBadge(r));

  // Detail table underneath — same data plus the timestamps +
  // last-error column the badge can't fit. Operators copy-paste
  // last_error into engine logs for triage; the table is the
  // direct surface for that.
  const tbl = h(
    "table",
    { class: "delivery-detail-table" },
    h(
      "thead",
      null,
      h(
        "tr",
        null,
        h("th", null, "Sink"),
        h("th", null, "Status"),
        h("th", null, "Attempts"),
        h("th", null, "Created"),
        h("th", null, "Delivered / next"),
        h("th", null, "Last error / suppression"),
      ),
    ),
    h("tbody", null, ...rows.map(deliveryDetailRow)),
  );

  return h("div", { class: "delivery-dialog-body" }, badgeStack, tbl);
}

const STATUS_ICONS: Record<OutboxStatus, string> = {
  pending: "•",
  sent: "✓",
  failed: "⚠",
  dead: "✕",
  suppressed: "⊘",
};

const STATUS_LABELS: Record<OutboxStatus, string> = {
  pending: "pending",
  sent: "sent",
  failed: "failed",
  dead: "dead",
  suppressed: "suppressed",
};

/// Human-readable label for each cascade-gate that can block a
/// delivery. Source of truth is the `SuppressionReason` enum in
/// `nexus-store::outbox`; the strings here are operator-facing
/// summaries that match the M7 doc's "off-schedule (global)" etc.
const SUPPRESSION_LABELS: Record<SuppressionReason, string> = {
  global_disabled: "global delivery disabled",
  rule_disabled: "rule delivery disabled",
  off_schedule_global: "off-schedule (global)",
  off_schedule_rule: "off-schedule (rule)",
};

function deliveryBadge(r: OutboxRow): HTMLElement {
  const icon = STATUS_ICONS[r.status];
  const summary = badgeSummary(r);
  const title = badgeTooltip(r);
  return h(
    "span",
    {
      class: `delivery-badge delivery-badge-${r.status}`,
      title,
    },
    h("strong", null, r.sink_id),
    " ",
    h("span", { class: "delivery-badge-icon" }, icon),
    " ",
    h("span", { class: "delivery-badge-summary" }, summary),
  );
}

function badgeSummary(r: OutboxRow): string {
  if (r.status === "suppressed" && r.suppression_reason) {
    return SUPPRESSION_LABELS[r.suppression_reason];
  }
  if (r.status === "failed" || r.status === "dead") {
    return `${r.attempts} attempt${r.attempts === 1 ? "" : "s"}`;
  }
  return STATUS_LABELS[r.status];
}

function badgeTooltip(r: OutboxRow): string {
  const bits: string[] = [];
  bits.push(`status=${r.status}`);
  bits.push(`attempts=${r.attempts}`);
  bits.push(`created=${r.created_at}`);
  if (r.delivered_at) bits.push(`delivered=${r.delivered_at}`);
  if (r.next_attempt_at) bits.push(`next=${r.next_attempt_at}`);
  if (r.suppression_reason) bits.push(`suppression=${r.suppression_reason}`);
  if (r.last_error) bits.push(`last_error=${r.last_error}`);
  return bits.join("  ·  ");
}

function deliveryDetailRow(r: OutboxRow): HTMLElement {
  const statusCell = h(
    "td",
    null,
    h(
      "span",
      { class: `delivery-status-pill delivery-status-${r.status}` },
      `${STATUS_ICONS[r.status]} ${STATUS_LABELS[r.status]}`,
    ),
  );
  // Combined "Delivered / next" column: sent rows surface their
  // delivered_at; failed / pending rows surface the upcoming
  // retry. Both ride the same column because the operator wants a
  // single "when next" answer per row.
  const deliveredCell =
    r.delivered_at !== null
      ? h(
          "td",
          { title: formatTimeTooltip(r.delivered_at) },
          formatLocalTime(r.delivered_at, "datetime"),
        )
      : r.next_attempt_at !== null
        ? h(
            "td",
            {
              class: "muted",
              title: `Next attempt at ${r.next_attempt_at}`,
            },
            `→ ${formatLocalTime(r.next_attempt_at, "datetime")}`,
          )
        : h("td", { class: "muted" }, "—");

  // Last-error / suppression text. Suppression rows have a
  // `suppression_reason` but typically no `last_error`; sent rows
  // have neither.
  const errText = r.last_error
    ? r.last_error
    : r.suppression_reason
      ? SUPPRESSION_LABELS[r.suppression_reason]
      : "";
  const errCell = errText
    ? h("td", { title: errText }, errText)
    : h("td", { class: "muted" }, "—");

  return h(
    "tr",
    null,
    h("td", null, h("code", null, r.sink_id)),
    statusCell,
    h("td", { class: "delivery-num" }, String(r.attempts)),
    h(
      "td",
      { title: formatTimeTooltip(r.created_at) },
      formatLocalTime(r.created_at, "datetime"),
    ),
    deliveredCell,
    errCell,
  );
}
