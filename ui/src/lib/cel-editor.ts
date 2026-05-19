// CEL editor with field + value autocomplete.
//
// Drop-in replacement for the plain `TextArea` in the rules form's
// "when (CEL)" field. Adds:
//   * Inline path completion driven by a hard-coded schema that
//     mirrors the canonical CEL context registered by
//     `crates/nexus-rules/src/lib.rs::{object_to_cel,
//     camera_to_cel, now_to_cel}`. Update both sides together
//     when adding new fields.
//   * Value-side completion for `object.label ==` / `!=` / `in`
//     (driven by the engine's prompt catalog if the caller passes
//     it in) and for `object.attributes['<key>']` (driven by a
//     small hard-coded list of the well-known attribute keys the
//     supervisor sets — motion.zone_state, motion.dwell_seconds,
//     motion.zone_ids).
//
// Intentionally NOT a full CEL parser — we use a lightweight
// caret-position regex to figure out what the operator is
// currently typing. Wrong guesses are silent (no popup), never
// destructive: the textarea remains the source of truth and
// validation still runs on blur via the existing
// `api.rules.validate` round-trip.

import { h } from "./el.js";
import type { FieldOpts } from "./forms.js";

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Each schema node is either a leaf (no `children`) or an object
/// (one child per keyable field). `desc` is shown in the
/// completion item; `type` is shown as a dim suffix.
interface CelNode {
  type: string;
  desc?: string;
  children?: Record<string, CelNode>;
}

/// Canonical CEL context. Mirror of
/// `crates/nexus-rules/src/lib.rs::{object_to_cel, camera_to_cel,
/// now_to_cel}`.
const CEL_SCHEMA: Record<string, CelNode> = {
  object: {
    type: "object",
    desc: "Tracked object on this frame",
    children: {
      label: {
        type: "string",
        desc: "Class label (e.g. person, vehicle.car)",
      },
      confidence: { type: "float", desc: "Detector score, 0.0–1.0" },
      track_id: { type: "int", desc: "Stable per-camera track id" },
      age_ms: { type: "int", desc: "Time since the track was born (ms)" },
      age_frames: { type: "int", desc: "Frames since the track was born" },
      box: {
        type: "object",
        desc: "Bounding box (normalised, 0..1 unless noted)",
        children: {
          x1: { type: "float", desc: "Left edge" },
          y1: { type: "float", desc: "Top edge" },
          x2: { type: "float", desc: "Right edge" },
          y2: { type: "float", desc: "Bottom edge" },
          width: { type: "float", desc: "x2 − x1" },
          height: { type: "float", desc: "y2 − y1" },
        },
      },
      attributes: {
        type: "map<string,string>",
        desc:
          "Annotator tags. Use object.attributes['key'] — auto-completes the well-known keys.",
      },
    },
  },
  camera: {
    type: "object",
    desc: "Camera context",
    children: {
      id: { type: "int", desc: "CameraId (matches camera_filter)" },
    },
  },
  now: {
    type: "object",
    desc: "Wall-clock time at evaluation",
    children: {
      unix_ms: { type: "int", desc: "Milliseconds since Unix epoch" },
      hour: { type: "int", desc: "0–23, local time" },
      day_of_week: { type: "int", desc: "1=Mon … 7=Sun" },
    },
  },
};

/// Well-known attribute keys the supervisor populates. Mirrors
/// the writes in `crates/nexus-pipeline/src/supervisor.rs`.
const KNOWN_ATTR_KEYS: ReadonlyArray<{ key: string; desc: string }> = [
  { key: "motion.zone_state", desc: "entering | inside | leaving" },
  { key: "motion.dwell_seconds", desc: "How long the object has been in-zone" },
  { key: "motion.zone_ids", desc: "Comma-joined list of zone ids" },
];

/// CEL keywords that show up at the top of the completion list
/// when the operator types something that isn't a known root.
const CEL_KEYWORDS: ReadonlyArray<{ keyword: string; desc: string }> = [
  { keyword: "true", desc: "Boolean literal" },
  { keyword: "false", desc: "Boolean literal" },
  { keyword: "null", desc: "Null literal" },
  { keyword: "in", desc: "Membership test — `'x' in list`" },
  { keyword: "&&", desc: "Logical AND" },
  { keyword: "||", desc: "Logical OR" },
  { keyword: "!", desc: "Logical NOT" },
];

// ---------------------------------------------------------------------------
// Completion model
// ---------------------------------------------------------------------------

interface Completion {
  /// Text inserted into the textarea (replaces `partial`).
  insert: string;
  /// What the operator sees in the dropdown.
  display: string;
  /// Optional type tag rendered dim on the right.
  type?: string;
  /// Optional one-line description rendered beneath the display.
  desc?: string;
  /// Set to `true` for completions that are likely to be followed
  /// by more input (e.g. an object that has children) — the
  /// caret stays put after insertion so the operator can keep
  /// typing without an extra dot.
  partial?: boolean;
}

export interface CelEditorOpts extends FieldOpts<string> {
  rows?: number;
  /// Fired on textarea blur. Same semantics as `TextArea`'s
  /// `onBlur` — the rules form uses it to run server-side
  /// validation without firing on every keystroke.
  onBlur?: (value: string) => void;
  /// When provided, used to drive value-completion for
  /// `object.label == '<here>'` etc. Each entry is a label the
  /// active detector kind is known to emit. Pass an empty array
  /// (or omit) to disable label-value completion.
  labelSuggestions?: ReadonlyArray<string>;
}

// ---------------------------------------------------------------------------
// Editor
// ---------------------------------------------------------------------------

export function CelEditor(opts: CelEditorOpts): HTMLElement {
  // ── Wiring overview ──────────────────────────────────────────
  // Layout: a positioning <div> wraps the <textarea> + a popup
  // <div>. The popup is absolutely positioned at the wrapper's
  // bottom-left (we don't track caret coordinates — overkill for
  // a 4-row textarea). Keyboard handling lives on the textarea so
  // Up/Down/Enter/Tab/Esc fire before the browser does anything
  // else with them. Mouse handling lives on the popup; we use
  // `mousedown` (NOT `click`) so the click handler fires BEFORE
  // the textarea's blur — without this the popup vanishes
  // mid-click and the insert never lands.

  const wrapper = h("div", { class: "cel-editor" });
  const popup = h("div", {
    class: "cel-completions",
    style: { display: "none" },
  });
  // ARIA combobox role so screen readers announce the popup.
  popup.setAttribute("role", "listbox");

  let completions: Completion[] = [];
  let active = 0;
  let suppressBlurValidate = false;

  const labelSuggestions = opts.labelSuggestions ?? [];

  const ta = h("textarea", {
    rows: opts.rows ?? 4,
    placeholder: opts.placeholder ?? "",
    required: !!opts.required,
    value: opts.value ?? "",
    class: "cel-editor-textarea",
    on: {
      input: (ev) => {
        const v = (ev.currentTarget as HTMLTextAreaElement).value;
        opts.onChange(v);
        refreshCompletions();
      },
      keyup: (ev) => {
        // Move-only keys (Home/End/arrows without selection) need
        // a recompute since `input` didn't fire.
        if (
          ev.key === "ArrowLeft" ||
          ev.key === "ArrowRight" ||
          ev.key === "Home" ||
          ev.key === "End"
        ) {
          refreshCompletions();
        }
      },
      keydown: (ev: KeyboardEvent) => {
        if (popup.style.display === "none" || completions.length === 0) return;
        switch (ev.key) {
          case "ArrowDown":
            ev.preventDefault();
            active = (active + 1) % completions.length;
            paintPopup();
            return;
          case "ArrowUp":
            ev.preventDefault();
            active = (active - 1 + completions.length) % completions.length;
            paintPopup();
            return;
          case "Enter":
          case "Tab":
            ev.preventDefault();
            applyCompletion(completions[active]!);
            return;
          case "Escape":
            ev.preventDefault();
            hidePopup();
            return;
          default:
            return;
        }
      },
      focus: () => refreshCompletions(),
      blur: (ev: FocusEvent) => {
        // Hide after a microtask so a popup click can still land.
        setTimeout(() => hidePopup(), 100);
        if (!suppressBlurValidate && opts.onBlur) {
          opts.onBlur((ev.currentTarget as HTMLTextAreaElement).value);
        }
        suppressBlurValidate = false;
      },
    },
  });

  function refreshCompletions(): void {
    const value = ta.value;
    const caret = ta.selectionStart;
    const ctx = computeCompletions(
      value,
      caret,
      labelSuggestions,
    );
    completions = ctx;
    if (completions.length === 0) {
      hidePopup();
      return;
    }
    active = 0;
    showPopup();
    paintPopup();
  }

  function showPopup(): void {
    popup.style.display = "block";
    ta.setAttribute("aria-expanded", "true");
  }

  function hidePopup(): void {
    popup.style.display = "none";
    ta.setAttribute("aria-expanded", "false");
  }

  function paintPopup(): void {
    while (popup.firstChild) popup.removeChild(popup.firstChild);
    completions.forEach((c, i) => {
      const item = h(
        "div",
        {
          class:
            "cel-completion-item" +
            (i === active ? " cel-completion-active" : ""),
          on: {
            // mousedown beats the textarea blur — see comment at top.
            mousedown: (ev: MouseEvent) => {
              ev.preventDefault();
              // Returning focus immediately also prevents the
              // outer dialog's autofocus from intercepting.
              suppressBlurValidate = true;
              applyCompletion(c);
            },
            mouseenter: () => {
              active = i;
              paintPopup();
            },
          },
        },
        h(
          "span",
          { class: "cel-completion-display" },
          c.display,
        ),
        c.type
          ? h("span", { class: "cel-completion-type" }, c.type)
          : null,
        c.desc
          ? h("div", { class: "cel-completion-desc" }, c.desc)
          : null,
      );
      item.setAttribute("role", "option");
      if (i === active) item.setAttribute("aria-selected", "true");
      popup.append(item);
    });
    // Scroll the active item into view if the popup overflows.
    const activeEl = popup.children[active] as HTMLElement | undefined;
    if (activeEl && typeof activeEl.scrollIntoView === "function") {
      activeEl.scrollIntoView({ block: "nearest" });
    }
  }

  function applyCompletion(c: Completion): void {
    const value = ta.value;
    const caret = ta.selectionStart;
    // Recompute the partial against the live caret so we know how
    // many chars to replace. Mirrors computeCompletions' regexes.
    const left = value.slice(0, caret);
    const right = value.slice(caret);
    const partial = matchPartial(left);
    const before = left.slice(0, left.length - partial.length);
    const next = before + c.insert + right;
    ta.value = next;
    const newCaret = (before + c.insert).length;
    ta.setSelectionRange(newCaret, newCaret);
    opts.onChange(next);
    ta.focus();
    // After an `object` insert (partial completion) the operator
    // usually wants to keep typing — re-run completions so the
    // child list shows up immediately for the next dot.
    if (c.partial) {
      // Synthesise a dot so the next refresh sees `object.`.
      refreshCompletions();
    } else {
      hidePopup();
    }
  }

  wrapper.append(ta, popup);

  // Field shell — reuse the same wrap structure as the rest of
  // the form primitives so the field reads identically to a
  // TextArea (label, helpText, error). We inline a minimal copy
  // of `wrap()` here rather than exporting it from forms.ts.
  const labelEl = h(
    "span",
    { class: "field-label" },
    opts.label,
    opts.required ? h("span", { class: "field-req" }, " *") : null,
  );
  const root = h(
    "label",
    { class: "field" + (opts.error ? " field-error" : "") },
    labelEl,
    wrapper,
    opts.helpText
      ? h("span", { class: "field-help" }, opts.helpText)
      : null,
    opts.error
      ? h("span", { class: "field-error-msg" }, opts.error)
      : null,
  );
  return root;
}

// ---------------------------------------------------------------------------
// Caret analysis
// ---------------------------------------------------------------------------

/// Return the in-progress identifier at the end of `left`, or
/// `""` if the caret is not inside one (e.g. after whitespace,
/// closing paren, etc.). Quoted strings also count as in-progress
/// when the value-completion branch decides to suggest values.
function matchPartial(left: string): string {
  // Value-side: ` == '<here>` — return everything inside the open quote.
  const stringMatch = left.match(/(['"])([^'"]*)$/);
  if (stringMatch) {
    return stringMatch[2]!;
  }
  // Identifier-side: `(prefix.)?partial` — return the partial only.
  const idMatch = left.match(/([A-Za-z_][\w]*)$/);
  if (idMatch) return idMatch[1]!;
  // Caret right after a dot → partial is empty, we still want to
  // suggest the children (so `object.` immediately pops the box).
  if (left.endsWith(".")) return "";
  return "";
}

interface PathContext {
  prefix: string; // dotted path up to (but not including) the partial
  partial: string;
}

function parsePath(left: string): PathContext | null {
  // Strip a trailing identifier; what's before (sans trailing dot)
  // is the prefix. If there's no trailing dot we treat the whole
  // expression as top-level.
  const m = left.match(/([A-Za-z_][\w]*(?:\.[A-Za-z_][\w]*)*)\.([A-Za-z_]\w*)?$/);
  if (m) {
    return { prefix: m[1]!, partial: m[2] ?? "" };
  }
  const top = left.match(/([A-Za-z_]\w*)?$/);
  if (top) return { prefix: "", partial: top[1] ?? "" };
  return null;
}

/// Walk the schema to find the node addressed by a dotted path.
function lookupPath(path: string): CelNode | null {
  if (path === "") return null;
  const parts = path.split(".");
  const root = CEL_SCHEMA[parts[0]!];
  if (!root) return null;
  let node: CelNode = root;
  for (let i = 1; i < parts.length; i++) {
    if (!node.children) return null;
    const next = node.children[parts[i]!];
    if (!next) return null;
    node = next;
  }
  return node;
}

// ---------------------------------------------------------------------------
// Completion computation
// ---------------------------------------------------------------------------

export function computeCompletions(
  value: string,
  caret: number,
  labelSuggestions: ReadonlyArray<string>,
): Completion[] {
  const left = value.slice(0, caret);

  // 1) Value-side completions — quoted-string context preceded by
  //    `==` / `!=` / `in`. Only branch when we can identify the
  //    subject; otherwise fall through to field completion.
  const stringMatch = left.match(/(['"])([^'"]*)$/);
  if (stringMatch) {
    const partial = stringMatch[2]!;
    // Look at the chunk to the LEFT of the open quote to figure
    // out which subject we're matching against.
    const beforeQuote = left.slice(0, left.length - partial.length - 1);
    const valueCompletions = valueCompletionsFor(beforeQuote, partial, labelSuggestions);
    if (valueCompletions !== null) return valueCompletions;
    // No known subject → no suggestions (don't fall through to
    // field completion — we're inside a string).
    return [];
  }

  // 2) Bracket-access completion: `object.attributes['<partial>'`
  //    is handled by the string branch above; this branch covers
  //    the moment the operator types the opening bracket but
  //    hasn't yet opened the quote (`object.attributes[`).
  if (/\.attributes\[\s*$/.test(left)) {
    return KNOWN_ATTR_KEYS.map((a) => ({
      insert: `'${a.key}']`,
      display: `'${a.key}'`,
      type: "attr key",
      desc: a.desc,
    }));
  }

  // 3) Field / path completion.
  const path = parsePath(left);
  if (!path) return [];
  if (path.prefix === "") {
    // Top-level: roots + keywords.
    const all: Completion[] = [];
    for (const [name, node] of Object.entries(CEL_SCHEMA)) {
      if (!startsWith(name, path.partial)) continue;
      const c: Completion = {
        insert: name,
        display: name,
        type: node.type,
      };
      if (node.desc !== undefined) c.desc = node.desc;
      if (node.children !== undefined) c.partial = true;
      all.push(c);
    }
    for (const k of CEL_KEYWORDS) {
      if (!startsWith(k.keyword, path.partial)) continue;
      all.push({
        insert: k.keyword,
        display: k.keyword,
        type: "keyword",
        desc: k.desc,
      });
    }
    return all;
  }

  // Nested: look up the prefix and suggest its children.
  const node = lookupPath(path.prefix);
  if (!node || !node.children) {
    // Special-case `object.attributes.<partial>` — the schema
    // marks attributes as a leaf map, but we can still suggest
    // the well-known keys as `attributes['key']` accessors.
    if (path.prefix === "object.attributes") {
      return KNOWN_ATTR_KEYS.filter((a) => startsWith(a.key, path.partial)).map(
        (a) => ({
          insert: `['${a.key}']`,
          display: `['${a.key}']`,
          type: "attr key",
          desc: a.desc,
        }),
      );
    }
    return [];
  }
  const out: Completion[] = [];
  for (const [name, child] of Object.entries(node.children)) {
    if (!startsWith(name, path.partial)) continue;
    const c: Completion = {
      insert: name,
      display: name,
      type: child.type,
    };
    if (child.desc !== undefined) c.desc = child.desc;
    if (child.children !== undefined) c.partial = true;
    out.push(c);
  }
  return out;
}

function startsWith(candidate: string, partial: string): boolean {
  if (partial === "") return true;
  return candidate.toLowerCase().startsWith(partial.toLowerCase());
}

function valueCompletionsFor(
  beforeQuote: string,
  partial: string,
  labelSuggestions: ReadonlyArray<string>,
): Completion[] | null {
  // Trim trailing whitespace + operator to find the subject.
  const trimmed = beforeQuote.replace(/\s+$/, "");
  // Bracket key — `object.attributes['` etc.
  const attrMatch = trimmed.match(/\.attributes\[\s*$/);
  if (attrMatch) {
    return KNOWN_ATTR_KEYS.filter((a) => startsWith(a.key, partial)).map((a) => ({
      insert: a.key,
      display: a.key,
      type: "attr key",
      desc: a.desc,
    }));
  }
  // `object.label == '`, `object.label != '`, `object.label in ['` …
  // Strip a leading `[` or `,` so list literals work too.
  const opMatch = trimmed.match(
    /([A-Za-z_][\w]*(?:\.[A-Za-z_][\w]*)+)\s*(==|!=|in)\s*(\[|,)?\s*$/,
  );
  if (opMatch) {
    const subject = opMatch[1]!;
    if (subject === "object.label" && labelSuggestions.length > 0) {
      return labelSuggestions
        .filter((l) => startsWith(l, partial))
        .map((l) => ({
          insert: l,
          display: l,
          type: "label",
        }));
    }
    if (subject === "object.attributes['motion.zone_state']") {
      return ["entering", "inside", "leaving"]
        .filter((v) => startsWith(v, partial))
        .map((v) => ({
          insert: v,
          display: v,
          type: "zone_state",
        }));
    }
    // Known subject but no value source — return null so the
    // caller knows not to suppress field completions (we're
    // still inside a string but we can't help).
    return [];
  }
  return null;
}
