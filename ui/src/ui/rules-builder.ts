// M-Admin Phase 4 — visual CEL builder for the Rules form.
//
// Renders a row-based subject ▸ operator ▸ value editor that
// compiles down to the same CEL string accepted by
// crates/nexus-rules. The shape is intentionally a flat list joined
// by a single AND/OR — anything more expressive (nested groups,
// mixed joiners) falls through to raw-mode editing in `rules-form`.
//
// Subject palette mirrors the CEL context registered by
// `crates/nexus-rules/src/lib.rs::{object_to_cel, camera_to_cel,
// now_to_cel}`. When adding new context fields there, mirror them
// here so the visual builder can target them.

import { h } from "../lib/el.js";
import { Combobox, NumberField, Select, TextField } from "../lib/forms.js";
import { COCO_DOMAIN_LABEL_STRINGS } from "../api/labels.js";

// ---------------------------------------------------------------------------
// Subject palette
// ---------------------------------------------------------------------------

/// How the value input should be rendered + what literal shape to
/// emit when compiling. Operators are derived from this in
/// [`opsForKind`].
export type ValueKind = "string" | "number" | "list_of_string";

export interface Subject {
  /// CEL accessor as it appears in compiled output.
  expr: string;
  /// Dropdown label (kept identical to `expr` for clarity — admins
  /// reading the corpus see exactly what hits the engine).
  label: string;
  kind: ValueKind;
  placeholder?: string;
  /// Closed set of allowed string values for enum-shaped subjects
  /// (e.g. `motion.zone_state`). Renders as a Select instead of
  /// a free-form TextField.
  options?: ReadonlyArray<string>;
  /// Open-ended suggestion list for string subjects. Rendered as
  /// a Combobox (text input + datalist + clickable chips) so the
  /// user can pick a common value with one click OR type a
  /// custom one. Used for `object.label` where the COCO domain
  /// labels cover ~99% of cases but yolo_world open-vocab
  /// pipelines can emit anything.
  suggestions?: ReadonlyArray<string>;
}

export const SUBJECTS: ReadonlyArray<Subject> = [
  {
    expr: "object.label",
    label: "object.label",
    kind: "string",
    placeholder: "person",
    suggestions: COCO_DOMAIN_LABEL_STRINGS,
  },
  { expr: "object.confidence", label: "object.confidence", kind: "number", placeholder: "0.7" },
  { expr: "object.track_id", label: "object.track_id", kind: "number" },
  { expr: "object.age_ms", label: "object.age_ms", kind: "number", placeholder: "1000" },
  { expr: "object.age_frames", label: "object.age_frames", kind: "number" },
  { expr: "object.box.x1", label: "object.box.x1", kind: "number", placeholder: "0.5" },
  { expr: "object.box.y1", label: "object.box.y1", kind: "number" },
  { expr: "object.box.x2", label: "object.box.x2", kind: "number" },
  { expr: "object.box.y2", label: "object.box.y2", kind: "number" },
  { expr: "object.box.width", label: "object.box.width", kind: "number" },
  { expr: "object.box.height", label: "object.box.height", kind: "number" },
  {
    expr: "object.attributes['motion.dwell_seconds']",
    label: "motion.dwell_seconds",
    kind: "number",
    placeholder: "60",
  },
  {
    expr: "object.attributes['motion.zone_state']",
    label: "motion.zone_state",
    kind: "string",
    options: ["entering", "inside", "leaving"],
  },
  {
    expr: "object.attributes['motion.zone_ids']",
    label: "motion.zone_ids",
    kind: "list_of_string",
    placeholder: "parking",
  },
  { expr: "camera.id", label: "camera.id", kind: "number" },
  { expr: "now.hour", label: "now.hour", kind: "number", placeholder: "0-23" },
  { expr: "now.day_of_week", label: "now.day_of_week", kind: "number", placeholder: "1=Mon … 7=Sun" },
  { expr: "now.unix_ms", label: "now.unix_ms", kind: "number" },
];

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

/// `eq..ge` map to the obvious CEL binary operators; `in_list` emits
/// `'X' in <subject>` to match the existing parking-zone corpus
/// pattern; `contains_substring` emits `<subject>.contains('X')`.
export type Op =
  | "eq"
  | "ne"
  | "lt"
  | "le"
  | "gt"
  | "ge"
  | "in_list"
  | "contains_substring";

const NUMERIC_OPS: ReadonlyArray<Op> = ["eq", "ne", "lt", "le", "gt", "ge"];
const STRING_OPS: ReadonlyArray<Op> = ["eq", "ne", "contains_substring"];
const LIST_OPS: ReadonlyArray<Op> = ["in_list"];

export function opsForKind(k: ValueKind): ReadonlyArray<Op> {
  switch (k) {
    case "number":
      return NUMERIC_OPS;
    case "string":
      return STRING_OPS;
    case "list_of_string":
      return LIST_OPS;
  }
}

export const OP_LABEL: Record<Op, string> = {
  eq: "==",
  ne: "!=",
  lt: "<",
  le: "<=",
  gt: ">",
  ge: ">=",
  in_list: "contains",
  contains_substring: "contains substring",
};

// ---------------------------------------------------------------------------
// Row + joiner model
// ---------------------------------------------------------------------------

export interface BuilderRow {
  /// Matches `Subject.expr`.
  subject: string;
  op: Op;
  /// Always stored as a string. Numeric subjects coerce on compile;
  /// invalid numerics fall back to "0".
  value: string;
}

export type Joiner = "and" | "or";

/// Look up a subject; falls back to the first entry so a row built
/// against a stale palette still renders something.
export function subjectFor(expr: string): Subject {
  return SUBJECTS.find((s) => s.expr === expr) ?? SUBJECTS[0]!;
}

// ---------------------------------------------------------------------------
// Compile (rows → CEL)
// ---------------------------------------------------------------------------

/// Compile a single row. Empty value on a numeric row coerces to 0;
/// empty value on a string row emits a literal empty string. Both
/// are valid CEL even if the result is unlikely to match — the
/// builder shouldn't crash on in-progress edits.
export function compileRow(row: BuilderRow): string {
  const subj = subjectFor(row.subject);
  switch (row.op) {
    case "in_list":
      return `${escString(row.value)} in ${subj.expr}`;
    case "contains_substring":
      return `${subj.expr}.contains(${escString(row.value)})`;
    case "eq":
    case "ne":
    case "lt":
    case "le":
    case "gt":
    case "ge": {
      const lit =
        subj.kind === "string"
          ? escString(row.value)
          : normaliseNumber(row.value);
      return `${subj.expr} ${OP_LABEL[row.op]} ${lit}`;
    }
  }
}

/// Compile a list of rows joined by a single joiner. Returns the
/// empty string for an empty row list — callers should treat that
/// as an unsaveable in-progress state.
export function compileBuilder(rows: ReadonlyArray<BuilderRow>, joiner: Joiner): string {
  if (rows.length === 0) return "";
  const sep = joiner === "or" ? " || " : " && ";
  return rows.map(compileRow).join(sep);
}

function escString(s: string): string {
  return "'" + s.replace(/\\/g, "\\\\").replace(/'/g, "\\'") + "'";
}

function normaliseNumber(raw: string): string {
  const t = raw.trim();
  if (t === "" || Number.isNaN(Number(t))) return "0";
  return t;
}

// ---------------------------------------------------------------------------
// Parse (CEL → rows) — best-effort, only handles the shape the
// builder itself emits. Anything else returns null and the caller
// falls back to raw-mode editing.
// ---------------------------------------------------------------------------

export interface ParsedBuilder {
  rows: BuilderRow[];
  joiner: Joiner;
}

const STRING_LIT_RE = "'((?:[^'\\\\]|\\\\.)*)'";

export function tryParseBuilder(when: string): ParsedBuilder | null {
  const trimmed = when.trim();
  if (trimmed === "") return { rows: [], joiner: "and" };

  // The compiler emits no parens, so plain string-split on the
  // joiner literals is sound. Mixed joiners can't be represented
  // by the flat row model — bail out so the user keeps raw mode.
  const hasAnd = /\s&&\s/.test(trimmed);
  const hasOr = /\s\|\|\s/.test(trimmed);
  if (hasAnd && hasOr) return null;
  const joiner: Joiner = hasOr ? "or" : "and";
  const parts = trimmed.split(joiner === "or" ? /\s\|\|\s/ : /\s&&\s/);

  const rows: BuilderRow[] = [];
  for (const part of parts) {
    const row = parseAtom(part.trim());
    if (!row) return null;
    rows.push(row);
  }
  return { rows, joiner };
}

function parseAtom(s: string): BuilderRow | null {
  // 'X' in <subject>
  {
    const m = s.match(new RegExp(`^${STRING_LIT_RE}\\s+in\\s+(.+)$`));
    if (m) {
      const subject = m[2]!.trim();
      if (SUBJECTS.find((x) => x.expr === subject)) {
        return { subject, op: "in_list", value: unescString(m[1]!) };
      }
    }
  }
  // <subject>.contains('X')
  {
    const m = s.match(new RegExp(`^(.+?)\\.contains\\(${STRING_LIT_RE}\\)$`));
    if (m) {
      const subject = m[1]!.trim();
      if (SUBJECTS.find((x) => x.expr === subject)) {
        return {
          subject,
          op: "contains_substring",
          value: unescString(m[2]!),
        };
      }
    }
  }
  // <subject> OP <value>
  {
    const m = s.match(/^(.+?)\s*(==|!=|<=|>=|<|>)\s*(.+)$/);
    if (m) {
      const subject = m[1]!.trim();
      const subj = SUBJECTS.find((x) => x.expr === subject);
      if (subj) {
        const opSym = m[2]!;
        const opMap = {
          "==": "eq",
          "!=": "ne",
          "<": "lt",
          "<=": "le",
          ">": "gt",
          ">=": "ge",
        } as const;
        const op = opMap[opSym as keyof typeof opMap] as Op;
        const raw = m[3]!.trim();
        if (subj.kind === "string") {
          const sm = raw.match(new RegExp(`^${STRING_LIT_RE}$`));
          if (!sm) return null;
          return { subject, op, value: unescString(sm[1]!) };
        }
        // numeric — must be a bare number literal
        if (!/^-?\d+(\.\d+)?$/.test(raw)) return null;
        return { subject, op, value: raw };
      }
    }
  }
  return null;
}

function unescString(s: string): string {
  return s.replace(/\\(.)/g, "$1");
}

// ---------------------------------------------------------------------------
// UI rendering
// ---------------------------------------------------------------------------

export interface BuilderUIOpts {
  rows: BuilderRow[];
  joiner: Joiner;
  /// Called whenever any field changes. The caller is expected to
  /// update its own state + re-run [`compileBuilder`] to refresh
  /// the linked textarea / preview.
  onChange: () => void;
  /// Notified when the AND/OR joiner select changes. The caller
  /// MUST write the new value into its own state — `opts.joiner`
  /// is a snapshot value, not a binding, so mutating it here
  /// would not propagate back. Without this hook the dropdown
  /// silently flipped back to AND on every recompile.
  onJoinerChange: (joiner: Joiner) => void;
}

export function renderBuilder(opts: BuilderUIOpts): HTMLElement {
  const host = h("div", { class: "rule-builder" });

  function rerender(): void {
    while (host.firstChild) host.removeChild(host.firstChild);
    host.append(buildInner(opts, rerender));
  }
  rerender();
  return host;
}

function buildInner(opts: BuilderUIOpts, rerender: () => void): HTMLElement {
  const wrap = h("div", null);

  if (opts.rows.length > 1) {
    wrap.append(
      h(
        "div",
        { class: "rule-builder-joiner-wrap" },
        Select<Joiner>({
          label: "Join with",
          value: opts.joiner,
          options: [
            { value: "and", label: "AND (all must match)" },
            { value: "or", label: "OR (any can match)" },
          ],
          onChange: (next) => {
            opts.joiner = next;
            opts.onJoinerChange(next);
            opts.onChange();
          },
        }),
      ),
    );
  }

  if (opts.rows.length === 0) {
    wrap.append(
      h(
        "div",
        { class: "rule-builder-empty" },
        "No conditions yet. Click ",
        h("strong", null, "+ Add condition"),
        " below to build a filter.",
      ),
    );
  } else {
    // Single column-header row above the stack of condition rows.
    // The rows themselves render with `hideLabel: true` on each
    // field so the heading isn't repeated 1× per row, which is
    // what made the builder feel busy.
    wrap.append(
      h(
        "div",
        { class: "rule-builder-row rule-builder-header" },
        h("span", { class: "rule-builder-col-label" }, "Subject"),
        h("span", { class: "rule-builder-col-label" }, "Operator"),
        h("span", { class: "rule-builder-col-label" }, "Value"),
        h("span", null),
      ),
    );
    for (let i = 0; i < opts.rows.length; i++) {
      wrap.append(buildRow(opts, i, rerender));
    }
  }

  // Footer actions: add / preview live below the form.
  wrap.append(
    h(
      "div",
      { class: "rule-builder-actions" },
      h(
        "button",
        {
          type: "button",
          class: "ghost rule-builder-add",
          on: {
            click: () => {
              opts.rows.push(defaultRow());
              opts.onChange();
              rerender();
            },
          },
        },
        "+ Add condition",
      ),
    ),
    h(
      "div",
      { class: "rule-builder-preview" },
      h("div", { class: "rule-builder-preview-label" }, "Compiled CEL"),
      h(
        "code",
        { class: "rule-builder-preview-code" },
        compileBuilder(opts.rows, opts.joiner) || "(no conditions)",
      ),
    ),
  );

  return wrap;
}

function buildRow(
  opts: BuilderUIOpts,
  index: number,
  rerender: () => void,
): HTMLElement {
  const row = opts.rows[index]!;
  const subj = subjectFor(row.subject);

  // Coerce the operator if the previous subject's ops don't include
  // the currently-selected one (happens when the user switches
  // subject from a string to a number, etc).
  const allowedOps = opsForKind(subj.kind);
  if (!allowedOps.includes(row.op)) {
    row.op = allowedOps[0]!;
  }

  // Per-row controls render in a compact column layout. We hide
  // the per-cell labels because the column-header row above
  // (rendered by `buildInner`) already announces Subject /
  // Operator / Value — repeating those labels on every row turns
  // the builder into a noisy stack of identical headings.
  const subjectSelect = Select<string>({
    label: "Subject",
    hideLabel: true,
    value: row.subject,
    options: SUBJECTS.map((s) => ({ value: s.expr, label: s.label })),
    onChange: (next) => {
      row.subject = next;
      // Clear the value when the kind changes — a number left over
      // in a string field is more confusing than a fresh blank.
      const newKind = subjectFor(next).kind;
      if (newKind !== subj.kind) row.value = "";
      opts.onChange();
      rerender();
    },
  });

  const opSelect = Select<Op>({
    label: "Operator",
    hideLabel: true,
    value: row.op,
    options: allowedOps.map((o) => ({ value: o, label: OP_LABEL[o] })),
    onChange: (next) => {
      row.op = next;
      opts.onChange();
      rerender();
    },
  });

  let valueField: HTMLElement;
  if (subj.options && subj.kind === "string") {
    // Bounded string — render as Select so the user can't typo
    // an invalid enum value.
    const currentOption = subj.options.includes(row.value)
      ? row.value
      : subj.options[0]!;
    if (row.value !== currentOption) row.value = currentOption;
    valueField = Select<string>({
      label: "Value",
      hideLabel: true,
      value: currentOption,
      options: subj.options.map((v) => ({ value: v, label: v })),
      onChange: (next) => {
        row.value = next;
        opts.onChange();
      },
    });
  } else if (subj.kind === "number") {
    const num = Number(row.value);
    valueField = NumberField({
      label: "Value",
      hideLabel: true,
      value: Number.isFinite(num) ? num : 0,
      ...(subj.placeholder !== undefined ? { placeholder: subj.placeholder } : {}),
      onChange: (next) => {
        row.value = String(next);
        opts.onChange();
      },
    });
  } else if (subj.suggestions && subj.suggestions.length > 0) {
    // Open-ended string with a known-good suggestion list (e.g.
    // `object.label` with the COCO catalogue). Combobox lets the
    // operator pick from the native datalist dropdown on focus,
    // OR type a custom value. We pass `hideChips: true` so the
    // always-visible chip strip doesn't pile 12 chips above every
    // row — the chooser stays available on the cameras form for
    // prompts, where it makes sense to see all options at once.
    valueField = Combobox({
      label: "Value",
      hideLabel: true,
      hideChips: true,
      value: row.value,
      suggestions: subj.suggestions,
      ...(subj.placeholder !== undefined ? { placeholder: subj.placeholder } : {}),
      onChange: (next) => {
        row.value = next;
        opts.onChange();
      },
    });
  } else {
    valueField = TextField({
      label: "Value",
      hideLabel: true,
      value: row.value,
      ...(subj.placeholder !== undefined ? { placeholder: subj.placeholder } : {}),
      onChange: (next) => {
        row.value = next;
        opts.onChange();
      },
    });
  }

  const removeBtn = h(
    "button",
    {
      type: "button",
      class: "ghost rule-builder-remove",
      title: "Remove this condition",
      on: {
        click: () => {
          opts.rows.splice(index, 1);
          opts.onChange();
          rerender();
        },
      },
    },
    "×",
  );

  return h(
    "div",
    { class: "rule-builder-row" },
    subjectSelect,
    opSelect,
    valueField,
    removeBtn,
  );
}

export function defaultRow(): BuilderRow {
  return { subject: "object.label", op: "eq", value: "person" };
}
