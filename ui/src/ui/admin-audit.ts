// M6 Phase 4 Step 4.3 — global /admin/audit page.
//
// Layout (single column, full main-pane width):
//
//   1. Header row — title.
//   2. Filter form — actor_id, action, resource_kind,
//      resource_id, outcome (dropdown), since/until
//      (datetime-local). "Apply" + "Reset" + "Refresh"
//      buttons; submitting the form (Enter) triggers Apply.
//   3. Audit table — When | Actor | Action | Resource |
//      Outcome | Details (expandable per-row JSON diff).
//      Same column layout as the per-resource history panel
//      so operators have one mental model.
//   4. Pagination — "Prev" + "Next" + page-size dropdown
//      (25 / 50 / 100). The backend caps at 500 per page.
//
// Authorisation: admin-only; `main.ts` should mark this entry
// with `requireAdmin: true`. Non-admins get a 403 from the API
// which renders an empty-state banner instead of crashing.
//
// Reuses styles from `audit-history.ts` so visual treatment
// matches (.audit-history-table, .audit-actor-*, .audit-outcome-*).

import { api } from "../api/client.js";
import { clear, h } from "../lib/el.js";
import type {
  AuditGlobalFilter,
  AuditOutcome,
  AuditPage,
  AuditRow,
} from "../api/types.js";

// Keep the on-screen filter state. Anything `null`/empty here
// means "do not include this filter in the API call".
interface FormState {
  actor_id: string;
  action: string;
  resource_kind: string;
  resource_id: string;
  outcome: "" | AuditOutcome;
  since: string;
  until: string;
  limit: number;
  offset: number;
}

const PAGE_SIZES = [25, 50, 100] as const;
const OUTCOMES: AuditOutcome[] = ["success", "failure", "denied"];

// Common resource_kind tokens emitted by the engine — surface
// as a datalist so operators have hints without locking the
// field (a new resource_kind shipped in a future release won't
// be missing from the picker).
const RESOURCE_KIND_HINTS = [
  "camera",
  "rule",
  "rule/delivery",
  "user",
  "admin/delivery",
  "admin/storage/cold",
  "admin/storage/backend",
  "admin/runtime/usb_preferred",
  "admin/oauth",
];

export async function renderAdminAudit(root: HTMLElement): Promise<void> {
  clear(root);

  const state: FormState = {
    actor_id: "",
    action: "",
    resource_kind: "",
    resource_id: "",
    outcome: "",
    since: "",
    until: "",
    limit: 50,
    offset: 0,
  };

  root.append(h("h2", null, "Audit log"));
  root.append(
    h(
      "p",
      { class: "muted" },
      "Every state-mutating admin action records a row here. Filter by actor, action, resource, or time window; expand a row to see the before/after JSON diff.",
    ),
  );

  const filterHost = h("section", { class: "admin-section audit-filter-host" });
  const tableHost = h("section", { class: "admin-section audit-table-host" });
  const pageHost = h("section", { class: "admin-section audit-page-host" });
  root.append(filterHost, tableHost, pageHost);

  function reload(): void {
    void loadAndRender(tableHost, pageHost, state);
  }

  renderFilterForm(filterHost, state, reload);
  reload();
}

// ---------------------------------------------------------------------------
// Filter form
// ---------------------------------------------------------------------------

function renderFilterForm(
  host: HTMLElement,
  state: FormState,
  reload: () => void,
): void {
  clear(host);
  host.append(h("h3", null, "Filters"));

  const form = h("form", {
    class: "audit-filter-form",
    on: {
      submit: (ev: Event) => {
        ev.preventDefault();
        state.offset = 0;
        reload();
      },
    },
  });

  // Field helper — keeps the markup readable.
  const field = (
    labelText: string,
    input: HTMLElement,
    hint?: string,
  ): HTMLElement =>
    h(
      "label",
      { class: "field" },
      h("span", { class: "field-label" }, labelText),
      input,
      hint ? h("span", { class: "field-hint muted" }, hint) : null,
    );

  // Resource kind datalist for autocomplete.
  const kindListId = "audit-resource-kind-list";
  const kindList = h("datalist", { id: kindListId });
  for (const k of RESOURCE_KIND_HINTS) {
    kindList.append(h("option", { value: k }));
  }

  const actorInput = h("input", {
    type: "text",
    value: state.actor_id,
    placeholder: "e.g. 1, 5, system",
    on: {
      input: (ev: Event) => {
        state.actor_id = (ev.target as HTMLInputElement).value.trim();
      },
    },
  });

  const actionInput = h("input", {
    type: "text",
    value: state.action,
    placeholder: "e.g. delivery.settings.put",
    on: {
      input: (ev: Event) => {
        state.action = (ev.target as HTMLInputElement).value.trim();
      },
    },
  });

  const kindInput = h("input", {
    type: "text",
    value: state.resource_kind,
    placeholder: "e.g. camera",
    on: {
      input: (ev: Event) => {
        state.resource_kind = (ev.target as HTMLInputElement).value.trim();
      },
    },
  });
  // `list` is a getter-only DOM property — wire via setAttribute.
  kindInput.setAttribute("list", kindListId);

  const resourceIdInput = h("input", {
    type: "text",
    value: state.resource_id,
    placeholder: "e.g. 7 or singleton",
    on: {
      input: (ev: Event) => {
        state.resource_id = (ev.target as HTMLInputElement).value.trim();
      },
    },
  });

  const outcomeSelect = h("select", {
    on: {
      change: (ev: Event) => {
        const v = (ev.target as HTMLSelectElement).value;
        state.outcome = v === "" ? "" : (v as AuditOutcome);
      },
    },
  });
  outcomeSelect.append(h("option", { value: "" }, "(any)"));
  for (const o of OUTCOMES) {
    const opt = h("option", { value: o }, o);
    if (state.outcome === o) opt.setAttribute("selected", "selected");
    outcomeSelect.append(opt);
  }

  const sinceInput = h("input", {
    type: "datetime-local",
    value: state.since,
    on: {
      input: (ev: Event) => {
        state.since = (ev.target as HTMLInputElement).value;
      },
    },
  });

  const untilInput = h("input", {
    type: "datetime-local",
    value: state.until,
    on: {
      input: (ev: Event) => {
        state.until = (ev.target as HTMLInputElement).value;
      },
    },
  });

  const limitSelect = h("select", {
    on: {
      change: (ev: Event) => {
        state.limit = parseInt((ev.target as HTMLSelectElement).value, 10);
        state.offset = 0;
      },
    },
  });
  for (const n of PAGE_SIZES) {
    const opt = h("option", { value: String(n) }, `${n} / page`);
    if (state.limit === n) opt.setAttribute("selected", "selected");
    limitSelect.append(opt);
  }

  const applyBtn = h(
    "button",
    { type: "submit", class: "primary" },
    "Apply",
  );

  const resetBtn = h(
    "button",
    {
      type: "button",
      class: "ghost",
      on: {
        click: () => {
          state.actor_id = "";
          state.action = "";
          state.resource_kind = "";
          state.resource_id = "";
          state.outcome = "";
          state.since = "";
          state.until = "";
          state.offset = 0;
          renderFilterForm(host, state, reload);
          reload();
        },
      },
    },
    "Reset",
  );

  const refreshBtn = h(
    "button",
    {
      type: "button",
      class: "ghost",
      on: {
        click: () => reload(),
      },
    },
    "Refresh",
  );

  form.append(
    kindList,
    h(
      "div",
      { class: "audit-filter-grid" },
      field("Actor id", actorInput, "match audit_log.actor_id"),
      field("Action", actionInput, "exact match"),
      field("Resource kind", kindInput, "e.g. camera, rule, user"),
      field("Resource id", resourceIdInput),
      field("Outcome", outcomeSelect),
      field("Since", sinceInput),
      field("Until", untilInput),
      field("Page size", limitSelect),
    ),
    h(
      "div",
      { class: "audit-filter-actions" },
      applyBtn,
      resetBtn,
      refreshBtn,
    ),
  );
  host.append(form);
}

// ---------------------------------------------------------------------------
// Table + pagination
// ---------------------------------------------------------------------------

async function loadAndRender(
  tableHost: HTMLElement,
  pageHost: HTMLElement,
  state: FormState,
): Promise<void> {
  clear(tableHost);
  clear(pageHost);
  tableHost.append(h("p", { class: "muted" }, "Loading audit rows…"));

  const filter: AuditGlobalFilter = { limit: state.limit, offset: state.offset };
  if (state.actor_id) filter.actor_id = state.actor_id;
  if (state.action) filter.action = state.action;
  if (state.resource_kind) filter.resource_kind = state.resource_kind;
  if (state.resource_id) filter.resource_id = state.resource_id;
  if (state.outcome) filter.outcome = state.outcome;
  if (state.since) filter.since = localToRfc3339(state.since);
  if (state.until) filter.until = localToRfc3339(state.until);

  let page: AuditPage;
  try {
    page = await api.adminAudit.list(filter);
  } catch (err) {
    clear(tableHost);
    const msg =
      err instanceof Error && /^403/.test(err.message)
        ? "You don't have permission to view the audit log (admin only)."
        : `Failed to load audit rows: ${err instanceof Error ? err.message : String(err)}`;
    tableHost.append(h("p", { class: "audit-history-error" }, msg));
    return;
  }

  clear(tableHost);
  if (page.rows.length === 0) {
    tableHost.append(
      h("p", { class: "muted" }, "No audit rows match the current filters."),
    );
  } else {
    renderTable(tableHost, page.rows);
  }
  renderPagination(pageHost, state, page.rows.length, () =>
    loadAndRender(tableHost, pageHost, state),
  );
}

function renderTable(host: HTMLElement, rows: AuditRow[]): void {
  const table = h("table", { class: "admin-table audit-history-table" });
  const thead = h(
    "thead",
    null,
    h(
      "tr",
      null,
      h("th", null, "When"),
      h("th", null, "Actor"),
      h("th", null, "Action"),
      h("th", null, "Resource"),
      h("th", null, "Outcome"),
      h("th", null, "Details"),
    ),
  );
  const tbody = h("tbody", null);
  for (const row of rows) {
    tbody.append(renderRow(row));
  }
  table.append(thead, tbody);
  host.append(table);
}

function renderRow(row: AuditRow): HTMLElement {
  const when = h(
    "td",
    { title: row.created_at },
    formatTimestamp(row.created_at),
  );
  const actor = h(
    "td",
    null,
    h(
      "span",
      { class: `audit-actor-kind audit-actor-${row.actor_kind}` },
      row.actor_kind.replace("_", " "),
    ),
    " ",
    h("span", { class: "audit-actor-label" }, row.actor_label || row.actor_id),
  );
  const action = h("td", null, h("code", null, row.action));
  const resource = h(
    "td",
    null,
    h("code", null, `${row.resource_kind}/${row.resource_id ?? "—"}`),
  );
  const outcome = h(
    "td",
    null,
    h(
      "span",
      { class: `audit-outcome audit-outcome-${row.outcome}` },
      row.outcome,
    ),
  );
  const details = h("td", { class: "audit-row-details" }, renderDetails(row));
  return h("tr", null, when, actor, action, resource, outcome, details);
}

function renderDetails(row: AuditRow): HTMLElement {
  const summary = h(
    "summary",
    { class: "audit-row-details-summary" },
    summariseChange(row),
  );
  const body = h("div", { class: "audit-row-details-body" });

  if (row.ip) {
    body.append(
      h(
        "p",
        { class: "muted" },
        `From ${row.ip}${row.user_agent ? ` · ${row.user_agent}` : ""}`,
      ),
    );
  }
  body.append(renderJsonDiff(row.before_json ?? null, row.after_json ?? null));

  return h("details", null, summary, body);
}

function renderJsonDiff(before: string | null, after: string | null): HTMLElement {
  if (!before && !after) {
    return h("p", { class: "muted" }, "no before/after recorded");
  }
  const pre = h("pre", { class: "audit-history-diff-pre" });
  const parts: string[] = [];
  if (before) {
    parts.push("--- before");
    parts.push(prettifyJson(before));
  }
  if (after) {
    if (parts.length) parts.push("");
    parts.push("+++ after");
    parts.push(prettifyJson(after));
  }
  pre.textContent = parts.join("\n");
  return pre;
}

function prettifyJson(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    // Not valid JSON — show raw so we never hide data from the operator.
    return raw;
  }
}

function summariseChange(row: AuditRow): string {
  if (row.before_json && !row.after_json) return "deleted";
  if (!row.before_json && row.after_json) return "created";
  if (row.before_json && row.after_json) return "updated";
  return row.outcome === "success" ? "no diff" : row.outcome;
}

function renderPagination(
  host: HTMLElement,
  state: FormState,
  rowCount: number,
  reload: () => void,
): void {
  clear(host);
  const prevDisabled = state.offset === 0;
  // Heuristic: if we got a full page, assume there's more.
  // The backend doesn't return a total count (cheap query first;
  // a COUNT(*) on a large audit_log defeats the index), so this
  // is the standard "next button disables on partial page" UX.
  const nextDisabled = rowCount < state.limit;
  const pageNumber = Math.floor(state.offset / Math.max(state.limit, 1)) + 1;

  const prev = h(
    "button",
    {
      type: "button",
      class: "ghost",
      on: {
        click: () => {
          state.offset = Math.max(0, state.offset - state.limit);
          reload();
        },
      },
    },
    "← Prev",
  );
  if (prevDisabled) prev.setAttribute("disabled", "true");

  const next = h(
    "button",
    {
      type: "button",
      class: "ghost",
      on: {
        click: () => {
          state.offset += state.limit;
          reload();
        },
      },
    },
    "Next →",
  );
  if (nextDisabled) next.setAttribute("disabled", "true");

  host.append(
    h(
      "div",
      { class: "audit-page-row" },
      prev,
      h(
        "span",
        { class: "muted" },
        `Page ${pageNumber} · showing ${rowCount} row${rowCount === 1 ? "" : "s"}`,
      ),
      next,
    ),
  );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatTimestamp(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString();
}

// Convert a `<input type="datetime-local">` value (no timezone)
// into an RFC3339 string in the operator's local timezone. We
// rely on the JS engine to do the offset arithmetic so an
// operator in PT typing 10:00 sees a `since` filter at 10:00 PT,
// not 10:00 UTC.
function localToRfc3339(local: string): string {
  if (!local) return "";
  const d = new Date(local);
  if (Number.isNaN(d.getTime())) return local;
  return d.toISOString();
}
