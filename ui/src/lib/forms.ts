// M-Admin Phase 0 — form primitives.
//
// Each builder returns an `HTMLElement` constructed via `h()`. Callers
// own the value state and pass an `onChange` callback. This avoids
// building any framework-style state machine: pages still hold their
// edited model in a plain object and re-render the form on save.
//
// Styling: see the "Form primitives" + "Chip input" sections of
// `ui/src/ui/styles.css` (Phase 0 block).

import { h } from "./el.js";
import { icon } from "./icons.js";

interface FieldMeta {
  helpText?: string;
  error?: string;
  required?: boolean;
  /// When `true`, the field renders without its label heading
  /// (and without the required-marker span). Used by dense
  /// table-style layouts — e.g. the rules visual builder, which
  /// repeats Subject/Operator/Value across N rows — where a
  /// single column-header row reads cleaner than a label per
  /// cell. `label` is still consumed for the `aria-label` so
  /// screen readers retain the announcement.
  hideLabel?: boolean;
}

export interface FieldOpts<T> extends FieldMeta {
  label: string;
  value: T;
  placeholder?: string;
  onChange: (next: T) => void;
}

export function TextField(
  opts: FieldOpts<string> & {
    /// Native `<input type=...>`. Defaults to `"text"`. Set to
    /// `"password"` to mask the value (used by the discovery
    /// dialog's shared-credentials block — the operator types one
    /// username/password that's then embedded into every camera
    /// URL added from that dialog).
    type?: "text" | "password";
    /// `autocomplete` attribute. Defaults to nothing for plain
    /// text fields; pass `"new-password"` when `type=password` to
    /// stop the browser pre-filling unrelated saved logins.
    autocomplete?: string;
    /// When `true` AND `type === "password"`, render a reveal
    /// toggle button inside the input that flips the field
    /// between masked + plaintext on click. Anywhere else (plain
    /// text fields) this option is a no-op. The toggle is local
    /// DOM state — no callback, no persistence, no logging — so
    /// the typed password never leaves the input.
    reveal?: boolean;
  },
): HTMLElement {
  const isPassword = opts.type === "password";
  const input = h("input", {
    type: opts.type ?? "text",
    value: opts.value ?? "",
    placeholder: opts.placeholder ?? "",
    required: !!opts.required,
    // Cast through `AutoFill` so TS's DOM lib accepts the value
    // (which is conceptually a token list but typed as a strict
    // string-literal union in DOM `HTMLInputElement.autocomplete`).
    ...(opts.autocomplete
      ? { autocomplete: opts.autocomplete as AutoFill }
      : {}),
    on: {
      input: (ev) => {
        opts.onChange((ev.currentTarget as HTMLInputElement).value);
      },
    },
  });

  // Plain text or password without reveal → bare input, same
  // legacy DOM shape callers have always seen.
  if (!isPassword || !opts.reveal) {
    return wrap(opts.label, input, opts);
  }

  // Password + reveal: wrap the `<input>` in a relative-positioned
  // span that anchors an absolutely-positioned eye toggle. Pure
  // DOM, no extra state — the button reads `input.type` directly,
  // flips it, and swaps the SVG icon child. CSS lives in the
  // "Password reveal toggle" block of `ui/src/ui/styles.css`.
  const eyeOpen = icon("eye");
  const eyeClosed = icon("eye-off");
  const toggleBtn = h(
    "button",
    {
      type: "button",
      class: "password-reveal",
      title: "Show password",
      on: {
        click: () => {
          const showing = input.type === "text";
          // Flip the input first, then swap the icon + ARIA so
          // they stay in sync even if the click handler throws.
          input.type = showing ? "password" : "text";
          toggleBtn.title = showing ? "Show password" : "Hide password";
          toggleBtn.setAttribute("aria-label", toggleBtn.title);
          while (toggleBtn.firstChild) {
            toggleBtn.removeChild(toggleBtn.firstChild);
          }
          toggleBtn.append(showing ? eyeOpen : eyeClosed);
        },
      },
    },
    eyeOpen,
  );
  toggleBtn.setAttribute("aria-label", "Show password");
  const inputWrap = h(
    "span",
    { class: "password-reveal-wrap" },
    input,
    toggleBtn,
  );
  return wrap(opts.label, inputWrap, opts);
}

/// Free-text input backed by a native `<datalist>` for autocomplete-
/// style suggestions. The user can pick a suggested value OR type
/// anything not in the list — perfect for `object.label` (most
/// people want a known COCO label, but yolo_world pipelines emit
/// arbitrary prompts and we can't lock them out).
///
/// We pair the input with a clickable chip strip ABOVE the input
/// for the same reason — `<datalist>` discoverability is poor on
/// some platforms (e.g. mobile Safari) so chips ensure the
/// suggestions are obvious even when the dropdown affordance is
/// missing.
export function Combobox(
  opts: FieldOpts<string> & {
    suggestions: ReadonlyArray<string>;
    /// Hide the chip strip even when suggestions are non-empty.
    /// Useful when there are too many suggestions to render
    /// inline (the datalist still works for autocomplete).
    hideChips?: boolean;
  },
): HTMLElement {
  // datalist id must be unique per call so a form with multiple
  // comboboxes doesn't share the same suggestion pool. Use the
  // counter + label to keep the markup readable in devtools.
  const id = `combobox-${++comboboxSeq}`;
  const input = h("input", {
    type: "text",
    value: opts.value ?? "",
    placeholder: opts.placeholder ?? "",
    required: !!opts.required,
    autocomplete: "off" as AutoFill,
    on: {
      input: (ev) => {
        opts.onChange((ev.currentTarget as HTMLInputElement).value);
      },
    },
  });
  // `HTMLInputElement.list` is typed as the resolved element, not
  // a string id — set it via attribute so TS doesn't complain and
  // the lookup still works.
  input.setAttribute("list", id);
  const datalist = h(
    "datalist",
    { id },
    ...opts.suggestions.map((s) => h("option", { value: s })),
  );

  const inputWrap = h("div", { class: "combobox-wrap" }, input, datalist);

  if (opts.hideChips || opts.suggestions.length === 0) {
    return wrap(opts.label, inputWrap, opts);
  }

  // Clickable chip strip — single-select, current value highlighted.
  // Click ⇒ overwrites the input value + fires onChange.
  const chipStrip = h("div", { class: "combobox-suggestions" });
  function renderChips(): void {
    while (chipStrip.firstChild) chipStrip.removeChild(chipStrip.firstChild);
    const current = (input as HTMLInputElement).value;
    for (const s of opts.suggestions) {
      const isOn = s === current;
      chipStrip.append(
        h(
          "button",
          {
            type: "button",
            class: "chip combobox-chip" + (isOn ? " chip-on" : ""),
            on: {
              click: () => {
                (input as HTMLInputElement).value = s;
                opts.onChange(s);
                renderChips();
              },
            },
          },
          s,
        ),
      );
    }
  }
  renderChips();
  // Re-highlight as the user types so the chip selection reflects
  // an exact-match typing.
  input.addEventListener("input", renderChips);

  const composite = h("div", { class: "combobox" }, chipStrip, inputWrap);
  return wrap(opts.label, composite, opts);
}

let comboboxSeq = 0;

export function NumberField(
  opts: FieldOpts<number> & { min?: number; max?: number; step?: number },
): HTMLElement {
  const input = h("input", {
    type: "number",
    value: String(opts.value ?? 0),
    placeholder: opts.placeholder ?? "",
    required: !!opts.required,
    on: {
      input: (ev) => {
        const raw = (ev.currentTarget as HTMLInputElement).value;
        const v = raw === "" ? 0 : Number(raw);
        opts.onChange(Number.isFinite(v) ? v : 0);
      },
    },
  });
  if (opts.min != null) input.min = String(opts.min);
  if (opts.max != null) input.max = String(opts.max);
  if (opts.step != null) input.step = String(opts.step);
  return wrap(opts.label, input, opts);
}

export function TextArea(
  opts: FieldOpts<string> & {
    rows?: number;
    /// M-Admin Phase 5 — optional blur callback so callers can run
    /// expensive validation (e.g. server-side CEL compile) without
    /// firing on every keystroke. Receives the current value.
    onBlur?: (value: string) => void;
  },
): HTMLElement {
  const ta = h("textarea", {
    rows: opts.rows ?? 4,
    placeholder: opts.placeholder ?? "",
    required: !!opts.required,
    value: opts.value ?? "",
    on: {
      input: (ev) => {
        opts.onChange((ev.currentTarget as HTMLTextAreaElement).value);
      },
      ...(opts.onBlur
        ? {
            blur: (ev: FocusEvent) => {
              opts.onBlur!((ev.currentTarget as HTMLTextAreaElement).value);
            },
          }
        : {}),
    },
  });
  return wrap(opts.label, ta, opts);
}

export function Toggle(opts: {
  label: string;
  value: boolean;
  onChange: (b: boolean) => void;
  helpText?: string;
}): HTMLElement {
  const input = h("input", {
    type: "checkbox",
    checked: opts.value,
    on: {
      change: (ev) =>
        opts.onChange((ev.currentTarget as HTMLInputElement).checked),
    },
  });
  return h(
    "label",
    { class: "field field-toggle" },
    input,
    h("span", { class: "field-label-inline" }, opts.label),
    opts.helpText ? h("span", { class: "field-help" }, opts.helpText) : null,
  );
}

export interface SelectOption<V extends string> {
  value: V;
  label: string;
}

export function Select<V extends string>(opts: {
  label: string;
  value: V;
  options: ReadonlyArray<SelectOption<V>>;
  helpText?: string;
  hideLabel?: boolean;
  onChange: (next: V) => void;
}): HTMLElement {
  const sel = h("select", {
    on: {
      change: (ev) => {
        opts.onChange((ev.currentTarget as HTMLSelectElement).value as V);
      },
    },
  });
  for (const o of opts.options) {
    const optEl = h("option", { value: o.value }, o.label);
    if (o.value === opts.value) optEl.selected = true;
    sel.append(optEl);
  }
  const meta: FieldMeta = {};
  if (opts.helpText !== undefined) meta.helpText = opts.helpText;
  if (opts.hideLabel) meta.hideLabel = true;
  return wrap(opts.label, sel, meta);
}

export function ChipsInput(opts: {
  label: string;
  value: string[];
  placeholder?: string;
  helpText?: string;
  /// Optional `<datalist>` source for type-ahead. Useful when the
  /// underlying detector has a baked vocabulary (open-vocab
  /// yolo_world): the operator can still type any string, but the
  /// browser surfaces the known prompts as autocomplete picks so
  /// they don't fat-finger a label the detector won't emit. Pass
  /// an empty array (or omit) to disable.
  suggestions?: ReadonlyArray<string>;
  onChange: (next: string[]) => void;
}): HTMLElement {
  const value = [...opts.value];
  const chips = h("div", { class: "chip-list" });

  function emit(): void {
    opts.onChange([...value]);
    renderChips();
  }

  function renderChips(): void {
    while (chips.firstChild) chips.removeChild(chips.firstChild);
    for (let i = 0; i < value.length; i++) {
      const v = value[i]!;
      const chip = h(
        "span",
        { class: "chip" },
        v,
        h(
          "button",
          {
            type: "button",
            class: "chip-x",
            title: `Remove ${v}`,
            on: {
              click: () => {
                const idx = value.indexOf(v);
                if (idx >= 0) {
                  value.splice(idx, 1);
                  emit();
                }
              },
            },
          },
          "✕",
        ),
      );
      chips.append(chip);
    }
  }

  // Stable, document-unique datalist id so multiple ChipsInputs
  // on the same form (e.g. prompts + tags) don't pull each other's
  // suggestions when the browser dedupes by id.
  const datalistId = `chips-suggest-${++datalistSeq}`;
  const input = h("input", {
    type: "text",
    placeholder: opts.placeholder ?? "Type and press Enter…",
    class: "chip-input-text",
    on: {
      keydown: (ev: KeyboardEvent) => {
        const target = ev.currentTarget as HTMLInputElement;
        if (ev.key === "Enter" || ev.key === ",") {
          ev.preventDefault();
          const v = target.value.trim();
          if (v && !value.includes(v)) {
            value.push(v);
            emit();
          }
          target.value = "";
        } else if (
          ev.key === "Backspace" &&
          target.value === "" &&
          value.length > 0
        ) {
          value.pop();
          emit();
        }
      },
    },
  });
  // `HTMLInputElement.list` is a getter-only DOM property — assigning
  // to it via h()'s prop bag throws `TypeError: Cannot set property
  // list ... has only a getter`. Wire the datalist via the attribute
  // instead.
  if (opts.suggestions && opts.suggestions.length > 0) {
    input.setAttribute("list", datalistId);
  }

  renderChips();
  const wrapperChildren: (HTMLElement | Node)[] = [chips, input];
  if (opts.suggestions && opts.suggestions.length > 0) {
    const dl = h("datalist", { id: datalistId });
    for (const s of opts.suggestions) {
      dl.append(h("option", { value: s }));
    }
    wrapperChildren.push(dl);
  }
  const wrapper = h("div", { class: "chip-input" }, ...wrapperChildren);
  const meta: FieldMeta = {};
  if (opts.helpText !== undefined) meta.helpText = opts.helpText;
  return wrap(opts.label, wrapper, meta);
}

// Module-private counter — survives across calls so the IDs are
// unique even when many ChipsInputs are mounted in the same dialog.
let datalistSeq = 0;

export function MultiSelect<V extends string>(opts: {
  label: string;
  value: V[];
  options: ReadonlyArray<SelectOption<V>>;
  helpText?: string;
  onChange: (next: V[]) => void;
}): HTMLElement {
  const selected = new Set<V>(opts.value);
  const list = h("div", { class: "chip-list" });

  function render(): void {
    while (list.firstChild) list.removeChild(list.firstChild);
    for (const o of opts.options) {
      const isOn = selected.has(o.value);
      const chip = h(
        "button",
        {
          type: "button",
          class: "chip" + (isOn ? " chip-on" : ""),
          on: {
            click: () => {
              if (selected.has(o.value)) selected.delete(o.value);
              else selected.add(o.value);
              opts.onChange(Array.from(selected));
              render();
            },
          },
        },
        o.label,
      );
      list.append(chip);
    }
  }
  render();

  const wrapper = h("div", { class: "chip-input" }, list);
  const meta: FieldMeta = {};
  if (opts.helpText !== undefined) meta.helpText = opts.helpText;
  return wrap(opts.label, wrapper, meta);
}

export function FormSection(title: string, ...children: (HTMLElement | null)[]): HTMLElement {
  const section = h("div", { class: "form-section" });
  section.append(h("h3", { class: "form-section-title" }, title));
  for (const c of children) if (c) section.append(c);
  return section;
}

export function FieldRow(...children: (HTMLElement | null)[]): HTMLElement {
  const row = h("div", { class: "field-row" });
  for (const c of children) if (c) row.append(c);
  return row;
}

function wrap(label: string, control: HTMLElement, meta: FieldMeta): HTMLElement {
  const cls = "field" + (meta.error ? " field-error" : "");
  if (meta.hideLabel) {
    // Apply the label as aria-label on the control itself so the
    // visual heading can be dropped without losing accessibility.
    if (!control.getAttribute("aria-label")) {
      control.setAttribute("aria-label", label);
    }
    return h(
      "div",
      { class: cls + " field-nolabel" },
      control,
      meta.helpText ? h("span", { class: "field-help" }, meta.helpText) : null,
      meta.error ? h("span", { class: "field-error-msg" }, meta.error) : null,
    );
  }
  return h(
    "label",
    { class: cls },
    h(
      "span",
      { class: "field-label" },
      label,
      meta.required ? h("span", { class: "field-req" }, " *") : null,
    ),
    control,
    meta.helpText ? h("span", { class: "field-help" }, meta.helpText) : null,
    meta.error ? h("span", { class: "field-error-msg" }, meta.error) : null,
  );
}
