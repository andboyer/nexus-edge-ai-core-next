// CodeMirror completion source for CEL.
//
// Context-sensitive: the source inspects the text immediately before
// the cursor to decide which slice of the schema to suggest.
//
//   1. After `.` on a known receiver  -> property completions.
//   2. After `[` on `object.attributes` -> quoted attribute-key keys.
//   3. After `== '` on a string field/key with an enum -> enum values.
//   4. At a top-level identifier      -> variables + stdlib funcs + snippets.
//
// The schema is in [`cel-schema.ts`]; this file owns parsing.

import {
  type Completion,
  type CompletionContext,
  type CompletionResult,
  type CompletionSource,
  snippetCompletion,
} from "@codemirror/autocomplete";

import {
  ATTRIBUTE_KEYS,
  type AttributeKeyCatalog,
  CEL_FUNCTIONS,
  CEL_SNIPPETS,
  CEL_VARIABLES,
  type CelProperty,
  OBJECT_TYPES,
} from "@/lib/cel-schema";

// ---------------------------------------------------------------------------
// External catalogs (label vocabulary, live attribute keys). Set by
// `<CelEditor>` from query data; reset between editor instances.
// ---------------------------------------------------------------------------

export interface CelExternalCatalogs {
  /** All detector labels currently emittable, sorted. Drives the
   *  string-literal suggestions on `object.label == '...'`. */
  labels?: readonly string[];
  /** Live attribute keys reported by the engine; merged with the
   *  static `motion` catalog. */
  liveAttributeKeys?: readonly string[];
}

// ---------------------------------------------------------------------------
// Top-level completion source.
// ---------------------------------------------------------------------------

export function makeCelCompletionSource(
  catalogs: CelExternalCatalogs = {},
): CompletionSource {
  return (ctx: CompletionContext): CompletionResult | null => {
    // Snapshot the line up to the cursor for cheap regex matching.
    const line = ctx.state.doc.lineAt(ctx.pos);
    const before = line.text.slice(0, ctx.pos - line.from);

    // ---- (3) string-literal completion after `== '` ----
    const enumMatch = matchEnumContext(before);
    if (enumMatch) {
      return enumCompletions(enumMatch, ctx.pos, catalogs);
    }

    // ---- (2) bracket-key completion on `object.attributes[` ----
    const bracketMatch = matchBracketKey(before);
    if (bracketMatch) {
      return bracketKeyCompletions(bracketMatch, ctx.pos, catalogs);
    }

    // ---- (1) dotted member completion ----
    const dotMatch = matchDottedAccess(before);
    if (dotMatch) {
      return dottedCompletions(dotMatch, ctx.pos);
    }

    // ---- (4) top-level identifier ----
    const wordMatch = ctx.matchBefore(/[A-Za-z_][\w]*$/);
    if (!wordMatch && !ctx.explicit) return null;
    const from = wordMatch ? wordMatch.from : ctx.pos;
    return topLevelCompletions(from);
  };
}

// ---------------------------------------------------------------------------
// (1) Dotted member access.
// ---------------------------------------------------------------------------

interface DottedMatch {
  /** Dotted receiver path, e.g. `object`, `object.box`. */
  receiverPath: string;
  /** The partial property name being typed after the last dot. */
  partial: string;
  /** Document position where the partial starts (for `from`). */
  partialFrom: number;
}

function matchDottedAccess(before: string): DottedMatch | null {
  // Capture (receiver)(.partial)$ — receiver is one+ identifier segments.
  // E.g. `object.bo` -> receiver=object, partial=bo
  //      `object.box.x` -> receiver=object.box, partial=x
  const m = before.match(/([A-Za-z_][\w]*(?:\.[A-Za-z_][\w]*)*)\.([A-Za-z_][\w]*)?$/);
  if (!m) return null;
  const receiverPath = m[1]!;
  const partial = m[2] ?? "";
  // `partialFrom` is relative to the END of `before`; caller will
  // convert to absolute positions.
  const partialFrom = before.length - partial.length;
  return { receiverPath, partial, partialFrom };
}

function dottedCompletions(
  match: DottedMatch,
  cursorAbs: number,
): CompletionResult | null {
  const props = propertiesOf(match.receiverPath);
  if (!props || props.length === 0) {
    // Also offer string methods if the receiver type is `string`.
    const t = typeOfPath(match.receiverPath);
    if (t === "string") {
      return {
        from: cursorAbs - match.partial.length,
        options: CEL_FUNCTIONS.filter((f) => f.isStringMethod).map((f) =>
          methodCompletion(f),
        ),
        validFor: /^[\w]*$/,
      };
    }
    return null;
  }
  const t = typeOfPath(match.receiverPath);
  const stringMethods =
    t === "string"
      ? CEL_FUNCTIONS.filter((f) => f.isStringMethod).map((f) => methodCompletion(f))
      : [];
  return {
    from: cursorAbs - match.partial.length,
    options: [
      ...props.map(propertyCompletion),
      ...stringMethods,
    ],
    validFor: /^[\w]*$/,
  };
}

// ---------------------------------------------------------------------------
// (2) Bracket-key completion (`object.attributes['<key>']`).
// ---------------------------------------------------------------------------

interface BracketKeyMatch {
  catalog: AttributeKeyCatalog;
  /** What's typed inside the brackets so far (without quotes). */
  partial: string;
  /** Quote style detected ('|"|none). */
  quote: "'" | '"' | "";
  /** Absolute position of the partial start. */
  partialAbsFrom: number;
}

function matchBracketKey(before: string): BracketKeyMatch | null {
  // E.g. `object.attributes['motion.sp` -> catalog=motion, partial=motion.sp
  // E.g. `object.attributes["g`         -> catalog=group (heuristic), partial=g
  // E.g. `object.attributes[`           -> catalog=motion (default)
  const m = before.match(
    /([A-Za-z_][\w]*(?:\.[A-Za-z_][\w]*)*)\[\s*(['"])?([\w.]*)$/,
  );
  if (!m) return null;
  const receiver = m[1]!;
  // Resolve receiver type — must be `map` to be a bracket-key situation.
  const recvType = typeOfPath(receiver);
  if (recvType !== "map") return null;
  // Default catalog: every receiver currently of type `map` is
  // `object.attributes`; future maps would pick their catalog via
  // CelProperty.keyCatalog. Look it up.
  const catalog = catalogOfPath(receiver) ?? "motion";
  const quote = (m[2] as "'" | '"' | undefined) ?? "";
  const partial = m[3] ?? "";
  // Compute where the partial *starts* (used as `from`). If a quote
  // was typed, partial starts right after it; if not, we're going to
  // insert the open quote ourselves, so `from` = current position.
  const partialAbsFrom =
    before.length -
    partial.length -
    (quote ? 0 : 0); /* placeholder; recomputed by caller */
  return { catalog, partial, quote, partialAbsFrom };
}

function bracketKeyCompletions(
  match: BracketKeyMatch,
  cursorAbs: number,
  catalogs: CelExternalCatalogs,
): CompletionResult {
  const staticKeys = ATTRIBUTE_KEYS[match.catalog] ?? [];
  const liveExtra = (catalogs.liveAttributeKeys ?? [])
    .filter((k) => !staticKeys.some((sk) => sk.name === k))
    .map((name) => ({ name, type: "string" as const, doc: "Engine-reported attribute." }));
  const all = [...staticKeys, ...liveExtra];

  // Determine the `from` position: cursor minus length of (partial + maybe-open-quote).
  const from = cursorAbs - match.partial.length - (match.quote ? 0 : 0);

  return {
    from,
    options: all.map((k) => {
      const openQuote = match.quote ? "" : "'";
      const closeQuote = match.quote ? `${match.quote}]` : "']";
      return {
        label: k.name,
        type: "property",
        detail: k.type,
        info: k.doc,
        apply: `${openQuote}${k.name}${closeQuote}`,
      } satisfies Completion;
    }),
    validFor: /^[\w.]*$/,
  };
}

// ---------------------------------------------------------------------------
// (3) String-literal enum completion after `==` / `!=`.
// ---------------------------------------------------------------------------

interface EnumMatch {
  /** Receiver path (e.g. `object.label`, `object.attributes['motion.speed_class']`). */
  receiver: string;
  /** The attribute key inside brackets, if the receiver was a map index. */
  attributeKey: string | null;
  /** What's typed inside the open quote so far. */
  partial: string;
  /** Quote style. */
  quote: "'" | '"';
}

function matchEnumContext(before: string): EnumMatch | null {
  // Three forms covered:
  //   object.label == 'pe                 -> receiver=object.label
  //   object.attributes['motion.x'] == 'r -> receiver=...attributes[...], attributeKey=motion.x
  //   object.attributes["motion.x"] != "r"
  const m = before.match(
    /([A-Za-z_][\w]*(?:\.[A-Za-z_][\w]*)*(?:\[\s*['"][\w.]+['"]\s*\])?)\s*[!=]=\s*(['"])([^'"]*)$/,
  );
  if (!m) return null;
  const receiver = m[1]!;
  const quote = m[2] as "'" | '"';
  const partial = m[3] ?? "";
  const bracketMatch = receiver.match(/\[\s*['"]([\w.]+)['"]\s*\]$/);
  const attributeKey = bracketMatch ? bracketMatch[1]! : null;
  return { receiver, attributeKey, partial, quote };
}

function enumCompletions(
  match: EnumMatch,
  cursorAbs: number,
  catalogs: CelExternalCatalogs,
): CompletionResult | null {
  let values: readonly string[] | undefined;

  if (match.attributeKey) {
    // Look up the attribute key's enum.
    for (const catKeys of Object.values(ATTRIBUTE_KEYS)) {
      for (const k of catKeys) {
        if (k.name === match.attributeKey) {
          values = k.enumValues;
          break;
        }
      }
      if (values) break;
    }
  } else {
    // Direct property — look it up in the schema.
    const propPath = match.receiver;
    // Special case: `object.label` falls back to the live label catalog.
    if (propPath === "object.label" && catalogs.labels) {
      values = catalogs.labels;
    } else {
      const dot = propPath.lastIndexOf(".");
      if (dot > 0) {
        const recv = propPath.slice(0, dot);
        const prop = propPath.slice(dot + 1);
        const props = propertiesOf(recv);
        const found = props?.find((p) => p.name === prop);
        values = found?.enumValues;
      }
    }
  }

  if (!values || values.length === 0) return null;

  const from = cursorAbs - match.partial.length;
  return {
    from,
    options: values.map((v) => ({
      label: v,
      type: "enum",
      apply: v,
    })),
    validFor: /^[^'"\s]*$/,
  };
}

// ---------------------------------------------------------------------------
// (4) Top-level identifier completion.
// ---------------------------------------------------------------------------

function topLevelCompletions(from: number): CompletionResult {
  const options: Completion[] = [];
  for (const v of CEL_VARIABLES) {
    options.push({
      label: v.name,
      type: "variable",
      detail: v.objectType ?? v.type,
      info: v.doc,
    });
  }
  for (const f of CEL_FUNCTIONS) {
    if (f.isStringMethod) continue; // only as a method
    options.push({
      label: f.name,
      type: "function",
      detail: f.signature,
      info: f.doc,
      apply: `${f.name}(`,
    });
  }
  for (const s of CEL_SNIPPETS) {
    options.push(
      snippetCompletion(s.template, {
        label: s.label,
        detail: s.detail,
        type: "text",
        boost: -10, // rank below variables/funcs
      }),
    );
  }
  return { from, options, validFor: /^[\w]*$/ };
}

// ---------------------------------------------------------------------------
// Schema lookup helpers.
// ---------------------------------------------------------------------------

/** Walk a dotted path from a root variable and return the type at the
 *  end. Returns `undefined` if any segment is unknown. */
function typeOfPath(path: string): CelProperty["type"] | "object" | undefined {
  const parts = path.split(".");
  const root = CEL_VARIABLES.find((v) => v.name === parts[0]);
  if (!root) return undefined;
  if (parts.length === 1) return root.type;
  let currentTypeKey = root.objectType;
  let lastProp: CelProperty | undefined;
  for (let i = 1; i < parts.length; i++) {
    if (!currentTypeKey) return undefined;
    const objType = OBJECT_TYPES[currentTypeKey];
    if (!objType) return undefined;
    lastProp = objType.properties.find((p) => p.name === parts[i]);
    if (!lastProp) return undefined;
    currentTypeKey = lastProp.objectType;
  }
  return lastProp?.type;
}

function propertiesOf(path: string): readonly CelProperty[] | undefined {
  const parts = path.split(".");
  const root = CEL_VARIABLES.find((v) => v.name === parts[0]);
  if (!root) return undefined;
  let currentTypeKey = root.objectType;
  for (let i = 1; i < parts.length; i++) {
    if (!currentTypeKey) return undefined;
    const objType = OBJECT_TYPES[currentTypeKey];
    if (!objType) return undefined;
    const next = objType.properties.find((p) => p.name === parts[i]);
    if (!next) return undefined;
    currentTypeKey = next.objectType;
  }
  if (!currentTypeKey) return undefined;
  return OBJECT_TYPES[currentTypeKey]?.properties;
}

function catalogOfPath(path: string): AttributeKeyCatalog | undefined {
  // Walk to the last property and return its keyCatalog.
  const parts = path.split(".");
  const root = CEL_VARIABLES.find((v) => v.name === parts[0]);
  if (!root || parts.length < 2) return undefined;
  let currentTypeKey = root.objectType;
  let lastProp: CelProperty | undefined;
  for (let i = 1; i < parts.length; i++) {
    if (!currentTypeKey) return undefined;
    const objType = OBJECT_TYPES[currentTypeKey];
    if (!objType) return undefined;
    lastProp = objType.properties.find((p) => p.name === parts[i]);
    if (!lastProp) return undefined;
    currentTypeKey = lastProp.objectType;
  }
  return lastProp?.keyCatalog;
}

// ---------------------------------------------------------------------------
// Completion factories.
// ---------------------------------------------------------------------------

function propertyCompletion(p: CelProperty): Completion {
  return {
    label: p.name,
    type: "property",
    detail: p.type,
    info: p.doc,
  };
}

function methodCompletion(f: { name: string; signature: string; doc: string }): Completion {
  return {
    label: f.name,
    type: "method",
    detail: f.signature,
    info: f.doc,
    apply: `${f.name}(`,
  };
}
