// M-Admin Phase 3 — Rules CRUD form.
//
// Sibling of `cameras-form.ts`: same dialog + form-primitive
// patterns, returns `true` on save / `false` on cancel. Covers every
// `RuleConfig` field exposed by the engine
// (`crates/nexus-config/src/lib.rs::RuleConfig`):
//
//   - id (string, readonly in edit mode)
//   - name
//   - severity (low | medium | high | critical)
//   - camera_filter (multi-select chips of cameras; empty = all)
//   - when (CEL — edited via a visual subject/op/value builder in
//           Phase 4, with an "Edit as text" escape hatch into the
//           raw textarea + the Phase 5 blur-validation hook)
//   - min_track_age_ms
//   - consecutive_frames
//   - cooldown_ms
//   - enabled
//
// Client-side validation is cheap and obvious — empty name / id,
// negative numbers, empty `when`. Anything subtler (bad CEL syntax,
// references to unknown fields) currently surfaces as a 500 on
// PUT / runtime crash, which is exactly the silent-failure gap
// Phase 5 will close.

import { api } from "../api/client.js";
import { h } from "../lib/el.js";
import { openDialog, dialogFooter, type DialogHandle } from "../lib/dialog.js";
import {
  TextField,
  NumberField,
  TextArea,
  Toggle,
  Select,
  MultiSelect,
  FormSection,
  FieldRow,
} from "../lib/forms.js";
import { toast } from "../lib/toast.js";
import {
  compileBuilder,
  defaultRow,
  renderBuilder,
  tryParseBuilder,
  type BuilderRow,
  type Joiner,
} from "./rules-builder.js";
import type {
  CameraConfig,
  CameraId,
  RuleConfig,
  RuleId,
  RulePreviewMatch,
  Severity,
} from "../api/types.js";

export interface OpenRuleFormOpts {
  mode: "create" | "edit";
  existing?: RuleConfig;
  /// All rule IDs currently in the table, used to flag duplicate-id
  /// collisions on create.
  existingIds: ReadonlyArray<RuleId>;
  /// All cameras currently configured, used to populate the
  /// camera_filter chip selector. We fetch this once in the parent
  /// and pass it through so the form can stay synchronous.
  cameras: ReadonlyArray<CameraConfig>;
}

interface FormState {
  id: string;
  name: string;
  severity: Severity;
  camera_filter: CameraId[];
  /// Zone-id allow-list. Each entry is the `id` of a `ZoneConfig`
  /// defined on some camera in `opts.cameras`. Empty = no gate.
  zones: string[];
  when: string;
  min_track_age_ms: number;
  consecutive_frames: number;
  cooldown_ms: number;
  enabled: boolean;
  // M-Admin Phase 4 — visual builder state. `editor_mode` is
  // initialised by trying to round-trip `when` through
  // `tryParseBuilder`; if that fails we stay in raw mode so the
  // expression can still be edited.
  editor_mode: "builder" | "raw";
  builder_rows: BuilderRow[];
  builder_joiner: Joiner;
}

const SEVERITY_OPTIONS: ReadonlyArray<{ value: Severity; label: string }> = [
  { value: "low", label: "low" },
  { value: "medium", label: "medium" },
  { value: "high", label: "high" },
  { value: "critical", label: "critical" },
];

const ID_RE = /^[a-z0-9_]+$/;
const CEL_HELP =
  "CEL expression. Available: object.{label,confidence,box,age_ms,age_frames,track_id,attributes[]}, camera.id, now.{hour,day_of_week,unix_ms}.";

export function openRuleForm(opts: OpenRuleFormOpts): Promise<boolean> {
  const state: FormState = buildInitialState(opts);

  let dlg: DialogHandle | null = null;
  let saving = false;

  const errors: Record<keyof FormState, string | undefined> = {
    id: undefined,
    name: undefined,
    severity: undefined,
    camera_filter: undefined,
    zones: undefined,
    when: undefined,
    min_track_age_ms: undefined,
    consecutive_frames: undefined,
    cooldown_ms: undefined,
    enabled: undefined,
    editor_mode: undefined,
    builder_rows: undefined,
    builder_joiner: undefined,
  };

  const formHost = h("div", { class: "rule-form" });

  // M-Admin Phase 5 — debounce + race-guard for the on-blur CEL
  // validation. Each blur bumps `validateSeq`; only the latest
  // response is allowed to mutate `errors.when` so a slow request
  // can't clobber a fresher result.
  let validateSeq = 0;
  async function validateWhen(value: string): Promise<void> {
    const seq = ++validateSeq;
    const trimmed = value.trim();
    if (trimmed.length === 0) {
      // Empty-string is caught by the synchronous required check on save.
      // Don't fire a network call for it; just clear any stale error.
      if (errors.when !== undefined) {
        errors.when = undefined;
        rerender();
      }
      return;
    }
    try {
      const res = await api.rules.validate(trimmed);
      if (seq !== validateSeq) return; // stale response
      const next = res.ok ? undefined : (res.error ?? "Invalid CEL expression.");
      if (errors.when !== next) {
        errors.when = next;
        rerender();
      }
    } catch {
      // Network failure — don't spam the user; Save will surface the
      // real upstream error if the engine is reachable then.
    }
  }

  function rerender(): void {
    while (formHost.firstChild) formHost.removeChild(formHost.firstChild);
    formHost.append(buildForm(state, errors, opts, validateWhen));
  }

  async function onSave(): Promise<void> {
    if (saving) return;
    if (!validate(state, opts, errors)) {
      rerender();
      toast.error("Fix the highlighted fields and try again.");
      return;
    }
    const payload = toRuleConfig(state);
    saving = true;
    try {
      await api.rules.upsert(payload);
      toast.success(
        opts.mode === "create"
          ? `Rule ${payload.id} added`
          : `Rule ${payload.id} saved`,
      );
      dlg?.close(true);
    } catch (err) {
      toast.error(`Save failed: ${(err as Error).message}`);
    } finally {
      saving = false;
    }
  }

  rerender();

  const footer = dialogFooter({
    cancelLabel: "Cancel",
    confirmLabel: opts.mode === "create" ? "Add rule" : "Save",
    onCancel: () => dlg?.close(false),
    onConfirm: () => void onSave(),
  });

  dlg = openDialog({
    title: opts.mode === "create" ? "Add rule" : `Edit rule ${state.id}`,
    body: formHost,
    footer,
    width: "640px",
  });

  return dlg.closed;
}

function buildInitialState(opts: OpenRuleFormOpts): FormState {
  if (opts.mode === "edit" && opts.existing) {
    const r = opts.existing;
    const parsed = tryParseBuilder(r.when);
    return {
      id: r.id,
      name: r.name,
      severity: (r.severity ?? "low") as Severity,
      camera_filter: r.camera_filter ? [...r.camera_filter] : [],
      zones: r.zones ? [...r.zones] : [],
      when: r.when,
      min_track_age_ms: r.min_track_age_ms ?? 500,
      consecutive_frames: r.consecutive_frames ?? 2,
      cooldown_ms: r.cooldown_ms ?? 30_000,
      enabled: r.enabled !== false,
      editor_mode: parsed ? "builder" : "raw",
      builder_rows: parsed ? parsed.rows : [],
      builder_joiner: parsed ? parsed.joiner : "and",
    };
  }
  const defaultRows: BuilderRow[] = [defaultRow()];
  return {
    id: "",
    name: "",
    severity: "low",
    camera_filter: [],
    zones: [],
    when: compileBuilder(defaultRows, "and"),
    min_track_age_ms: 500,
    consecutive_frames: 2,
    cooldown_ms: 30_000,
    enabled: true,
    editor_mode: "builder",
    builder_rows: defaultRows,
    builder_joiner: "and",
  };
}

function buildForm(
  state: FormState,
  errors: Record<string, string | undefined>,
  opts: OpenRuleFormOpts,
  validateWhen: (value: string) => void,
): HTMLElement {
  const idField =
    opts.mode === "edit"
      ? readOnlyField("ID", state.id)
      : TextField({
          label: "ID",
          value: state.id,
          required: true,
          placeholder: "loitering_after_hours",
          helpText: "Lowercase letters, digits, and underscores. Must be unique.",
          ...(errors["id"] !== undefined ? { error: errors["id"] } : {}),
          onChange: (v) => {
            state.id = v.trim();
          },
        });
  return h(
    "div",
    null,
    FormSection(
      "Basics",
      FieldRow(
        idField,
        TextField({
          label: "Name",
          value: state.name,
          required: true,
          placeholder: "Loitering after hours",
          ...(errors["name"] !== undefined ? { error: errors["name"] } : {}),
          onChange: (v) => {
            state.name = v;
          },
        }),
      ),
      FieldRow(
        Select<Severity>({
          label: "Severity",
          value: state.severity,
          options: SEVERITY_OPTIONS,
          helpText: "Drives alert routing + colour in the events feed.",
          onChange: (next) => {
            state.severity = next;
          },
        }),
        Toggle({
          label: "Enabled",
          value: state.enabled,
          helpText: "Disable to keep the rule but stop firing alerts.",
          onChange: (b) => {
            state.enabled = b;
          },
        }),
      ),
      MultiSelect<string>({
        label: "Camera filter",
        value: state.camera_filter.map((id) => String(id)),
        options: opts.cameras.map((c) => ({
          value: String(c.id),
          label: `${c.id} · ${c.name}`,
        })),
        helpText:
          "Restrict the rule to specific cameras. Leave all chips off = applies to every camera.",
        onChange: (next) => {
          state.camera_filter = next
            .map((s) => Number(s))
            .filter((n) => Number.isFinite(n));
          // Drop any selected zones that belong to cameras we just
          // removed from the filter, so the rule stays consistent.
          state.zones = pruneZonesAgainstFilter(
            state.zones,
            state.camera_filter,
            opts.cameras,
          );
        },
      }),
      MultiSelect<string>({
        label: "Zones",
        value: [...state.zones],
        options: zoneOptionsForCameras(opts.cameras, state.camera_filter),
        helpText:
          "Restrict the rule to objects whose bbox centre falls inside one of these zones on the camera. Zones must be defined on the camera (use the Cameras tab → Edit → Zones). Leave empty = no zone gate.",
        onChange: (next) => {
          state.zones = next;
        },
      }),
    ),
    FormSection(
      "Condition",
      buildConditionSection(state, errors, validateWhen),
    ),
    FormSection(
      "Debounce",
      FieldRow(
        NumberField({
          label: "Min track age (ms)",
          value: state.min_track_age_ms,
          min: 0,
          step: 100,
          helpText: "Track must exist this long before the rule fires.",
          ...(errors["min_track_age_ms"] !== undefined
            ? { error: errors["min_track_age_ms"] }
            : {}),
          onChange: (v) => {
            state.min_track_age_ms = Math.max(0, Math.floor(v));
          },
        }),
        NumberField({
          label: "Consecutive frames",
          value: state.consecutive_frames,
          min: 1,
          step: 1,
          helpText:
            "Predicate must hold for this many consecutive frames per track.",
          ...(errors["consecutive_frames"] !== undefined
            ? { error: errors["consecutive_frames"] }
            : {}),
          onChange: (v) => {
            state.consecutive_frames = Math.max(1, Math.floor(v));
          },
        }),
        NumberField({
          label: "Cooldown (ms)",
          value: state.cooldown_ms,
          min: 0,
          step: 1000,
          helpText:
            "After firing, the same rule+track cannot re-fire for this long.",
          ...(errors["cooldown_ms"] !== undefined
            ? { error: errors["cooldown_ms"] }
            : {}),
          onChange: (v) => {
            state.cooldown_ms = Math.max(0, Math.floor(v));
          },
        }),
      ),
    ),
    FormSection("Preview", buildPreviewSection(state, opts.cameras)),
  );
}

function readOnlyField(label: string, value: string): HTMLElement {
  return h(
    "label",
    { class: "field" },
    h("span", { class: "field-label" }, label),
    h("input", { type: "text", value, disabled: true, readOnly: true }),
    h(
      "span",
      { class: "field-help" },
      "Rule id cannot be changed after creation.",
    ),
  );
}

/// Condition section — owns the builder ↔ raw mode toggle. Both
/// modes write into `state.when`; the outer save flow doesn't need
/// to know which one was used. The two buttons rerender locally so
/// the rest of the form (id, name, debounce knobs) keeps its DOM
/// state.
function buildConditionSection(
  state: FormState,
  errors: Record<string, string | undefined>,
  validateWhen: (value: string) => void,
): HTMLElement {
  const host = h("div", { class: "condition-section" });

  function inner(): void {
    while (host.firstChild) host.removeChild(host.firstChild);

    const tabs = h(
      "div",
      { class: "condition-mode-tabs" },
      h(
        "button",
        {
          type: "button",
          class:
            "ghost condition-mode-tab" +
            (state.editor_mode === "builder" ? " active" : ""),
          on: { click: () => switchTo("builder") },
        },
        "Builder",
      ),
      h(
        "button",
        {
          type: "button",
          class:
            "ghost condition-mode-tab" +
            (state.editor_mode === "raw" ? " active" : ""),
          on: { click: () => switchTo("raw") },
        },
        "Edit as text",
      ),
    );

    const body: HTMLElement =
      state.editor_mode === "builder"
        ? renderBuilder({
            rows: state.builder_rows,
            joiner: state.builder_joiner,
            onJoinerChange: (j) => {
              state.builder_joiner = j;
            },
            onChange: () => {
              state.when = compileBuilder(
                state.builder_rows,
                state.builder_joiner,
              );
              // The builder shape is always syntactically valid;
              // clear any stale error from a prior raw-mode edit.
              if (errors["when"] !== undefined) errors["when"] = undefined;
            },
          })
        : TextArea({
            label: "when (CEL)",
            value: state.when,
            rows: 4,
            required: true,
            placeholder:
              "object.label == 'person' && object.attributes['motion.dwell_seconds'] >= 60",
            helpText: CEL_HELP,
            ...(errors["when"] !== undefined ? { error: errors["when"] } : {}),
            onChange: (v) => {
              state.when = v;
            },
            onBlur: (v) => {
              state.when = v;
              void validateWhen(v);
            },
          });

    host.append(tabs, body);
  }

  function switchTo(mode: "builder" | "raw"): void {
    if (mode === state.editor_mode) return;
    if (mode === "builder") {
      const parsed = tryParseBuilder(state.when);
      if (!parsed) {
        toast.error(
          "This expression can't be represented in the visual builder. Stay in text mode or simplify it.",
        );
        return;
      }
      state.builder_rows = parsed.rows;
      state.builder_joiner = parsed.joiner;
      state.when = compileBuilder(parsed.rows, parsed.joiner);
      if (errors["when"] !== undefined) errors["when"] = undefined;
    } else {
      // builder → raw: surface the live-compiled CEL so the
      // textarea picks it up as the starting point.
      state.when = compileBuilder(state.builder_rows, state.builder_joiner);
    }
    state.editor_mode = mode;
    inner();
  }

  inner();
  return host;
}

function validate(
  state: FormState,
  opts: OpenRuleFormOpts,
  errors: Record<string, string | undefined>,
): boolean {
  for (const k of Object.keys(errors)) errors[k] = undefined;
  let ok = true;

  if (opts.mode === "create") {
    const id = state.id.trim();
    if (id === "") {
      errors["id"] = "ID is required.";
      ok = false;
    } else if (!ID_RE.test(id)) {
      errors["id"] = "Only lowercase letters, digits, and underscores.";
      ok = false;
    } else if (opts.existingIds.includes(id)) {
      errors["id"] = `ID '${id}' is already in use.`;
      ok = false;
    }
  }

  if (state.name.trim() === "") {
    errors["name"] = "Name is required.";
    ok = false;
  }

  if (state.when.trim() === "") {
    errors["when"] = "CEL expression is required.";
    ok = false;
  }

  if (state.min_track_age_ms < 0) {
    errors["min_track_age_ms"] = "Cannot be negative.";
    ok = false;
  }
  if (state.consecutive_frames < 1) {
    errors["consecutive_frames"] = "Must be at least 1.";
    ok = false;
  }
  if (state.cooldown_ms < 0) {
    errors["cooldown_ms"] = "Cannot be negative.";
    ok = false;
  }

  return ok;
}

function toRuleConfig(state: FormState): RuleConfig {
  return {
    id: state.id.trim(),
    name: state.name.trim(),
    severity: state.severity,
    camera_filter:
      state.camera_filter.length === 0 ? null : [...state.camera_filter],
    zones: state.zones.length === 0 ? null : [...state.zones],
    when: state.when.trim(),
    min_track_age_ms: state.min_track_age_ms,
    consecutive_frames: state.consecutive_frames,
    cooldown_ms: state.cooldown_ms,
    enabled: state.enabled,
  };
}

/// Build the option list for the Zones MultiSelect: the union of
/// every zone defined on the cameras currently in scope (i.e. the
/// camera_filter selection — or all cameras if the filter is
/// empty). Each option label is `<camera name>: <zone name>` so
/// operators can disambiguate when two cameras define a zone with
/// the same name.
///
/// Option values are the bare zone ids — the rule config stores
/// ids only and looks them up against the camera's zones at
/// evaluation time. We do NOT prefix the value with the camera id
/// because the engine's lookup is per-camera anyway (the active
/// camera at evaluation time is the producer of the event, and
/// the zone gate only consults *its* zone list).
function zoneOptionsForCameras(
  cameras: ReadonlyArray<CameraConfig>,
  cameraFilter: ReadonlyArray<CameraId>,
): { value: string; label: string }[] {
  const inScope =
    cameraFilter.length === 0
      ? cameras
      : cameras.filter((c) => cameraFilter.includes(c.id));
  const opts: { value: string; label: string }[] = [];
  for (const cam of inScope) {
    if (!cam.zones || cam.zones.length === 0) continue;
    for (const z of cam.zones) {
      opts.push({
        value: z.id,
        label: `${cam.name}: ${z.name || z.id}`,
      });
    }
  }
  // De-duplicate by value — if two cameras share a zone id (rare
  // but possible after copy-pasting zones), keep the first label
  // we encountered. The engine still resolves per-camera so the
  // behaviour is correct either way.
  const seen = new Set<string>();
  return opts.filter((o) => {
    if (seen.has(o.value)) return false;
    seen.add(o.value);
    return true;
  });
}

/// Drop zone ids from `selected` that no longer correspond to any
/// zone on the in-scope cameras. Called from the camera_filter
/// onChange so deselecting a camera also drops its zones from the
/// rule, avoiding a "phantom" zone gate that silently never
/// matches.
function pruneZonesAgainstFilter(
  selected: ReadonlyArray<string>,
  cameraFilter: ReadonlyArray<CameraId>,
  cameras: ReadonlyArray<CameraConfig>,
): string[] {
  const validIds = new Set(
    zoneOptionsForCameras(cameras, cameraFilter).map((o) => o.value),
  );
  return selected.filter((id) => validIds.has(id));
}

/// "What would this rule have fired on?" — runs the candidate
/// rule against the last 24h of motion_events (the per-detection
/// table written by the live pipeline) and lists the matches
/// inline. Lets the operator tune CEL + zones against real data
/// before saving the rule.
///
/// The preview deliberately bypasses debounce/cooldown gates —
/// it shows raw predicate matches so those knobs can be tuned
/// independently. The hint text below the button calls that out.
function buildPreviewSection(
  state: FormState,
  cameras: ReadonlyArray<CameraConfig>,
): HTMLElement {
  const cameraNameById = new Map<CameraId, string>(
    cameras.map((c) => [c.id, c.name]),
  );

  // Results host — replaced wholesale per run. Keeping a single
  // container (not a re-render of the whole section) means the
  // button stays mounted and focused after the click.
  const resultsHost = h("div", { class: "rule-preview-results" });
  const summaryHost = h("div", { class: "rule-preview-summary field-help" });

  let inFlight = false;
  const runBtn = h(
    "button",
    {
      type: "button",
      class: "btn primary",
      on: {
        click: () => {
          if (inFlight) return;
          // Validate the current `when` cheaply before sending —
          // the engine catches it too and returns it as `error`,
          // but failing fast here saves a round-trip.
          const when = state.when.trim();
          if (!when) {
            summaryHost.textContent =
              "Add a CEL expression in the Condition section first.";
            resultsHost.replaceChildren();
            return;
          }
          inFlight = true;
          runBtn.disabled = true;
          runBtn.textContent = "Running…";
          summaryHost.textContent = "";
          resultsHost.replaceChildren();
          // Build the rule from current form state. We don't save
          // it — the engine compiles it transiently per request.
          const rule = (() => {
            try {
              return toPreviewRule(state);
            } catch {
              return null;
            }
          })();
          if (rule === null) {
            inFlight = false;
            runBtn.disabled = false;
            runBtn.textContent = "Run preview (last 24h)";
            summaryHost.textContent = "Rule is incomplete — fill in the form first.";
            return;
          }
          void api.rules
            .preview({ rule, limit: 500 })
            .then((resp) => {
              inFlight = false;
              runBtn.disabled = false;
              runBtn.textContent = "Run preview (last 24h)";
              if (resp.error) {
                summaryHost.textContent = `CEL error: ${resp.error}`;
                resultsHost.replaceChildren();
                return;
              }
              const limited = resp.limit_hit
                ? ` · scan capped at ${resp.scanned} rows — widen the window or narrow the camera filter to see more`
                : "";
              summaryHost.textContent =
                resp.matches.length === 0
                  ? `No matches in the last 24h (scanned ${resp.scanned} detections).${limited}`
                  : `${resp.matches.length} match${resp.matches.length === 1 ? "" : "es"} in the last 24h (scanned ${resp.scanned} detections).${limited}`;
              if (resp.matches.length === 0 && resp.scanned > 0) {
                // Zero-match diagnosis: surface the labels the
                // detector actually emitted in the window. The
                // most common reason a 'vehicle' rule reports
                // zero matches is that the COCO mapper namespaces
                // labels as `vehicle.car`, `vehicle.truck`, etc.
                // — showing the histogram makes that obvious in
                // one glance, no log-reading required.
                resultsHost.replaceChildren(
                  renderScannedLabelsHint(
                    resp.scanned_labels ?? [],
                    state.when,
                    resp.eval_errors ?? 0,
                    resp.eval_first_error,
                    resp.effective_when,
                    resp.zone_filtered ?? 0,
                  ),
                );
              } else {
                resultsHost.replaceChildren(
                  renderPreviewMatches(resp.matches, cameraNameById),
                );
              }
            })
            .catch((err: unknown) => {
              inFlight = false;
              runBtn.disabled = false;
              runBtn.textContent = "Run preview (last 24h)";
              const msg = err instanceof Error ? err.message : String(err);
              summaryHost.textContent = `Preview failed: ${msg}`;
              resultsHost.replaceChildren();
              toast.error(`Preview failed: ${msg}`);
            });
        },
      },
    },
    "Run preview (last 24h)",
  );

  const hint = h(
    "div",
    { class: "field-help" },
    "Replays the current rule against detections from the last 24h. Debounce / cooldown gates are NOT applied — preview shows raw predicate matches so you can tune the CEL + zones in isolation. ",
    h(
      "code",
      null,
      "object.age_ms",
    ),
    " reads as 0 in preview (track age can't be reconstructed from a single past row).",
  );

  return h(
    "div",
    { class: "rule-preview-section" },
    h("div", { class: "rule-preview-controls" }, runBtn),
    hint,
    summaryHost,
    resultsHost,
  );
}

/// Render the matches list as a compact table. `<a>` on each row
/// deep-links to the existing clips view via the hash route the
/// rest of the SPA uses.
function renderPreviewMatches(
  matches: ReadonlyArray<RulePreviewMatch>,
  cameraNameById: ReadonlyMap<CameraId, string>,
): HTMLElement {
  if (matches.length === 0) {
    return h("div", null);
  }
  const rows = matches.map((m) => {
    const camName =
      cameraNameById.get(m.camera_id) ?? `camera ${m.camera_id}`;
    const ts = formatRelativeTime(m.captured_at);
    return h(
      "tr",
      null,
      h("td", null, ts),
      h("td", null, camName),
      h("td", null, m.label),
      h("td", null, `${(m.confidence * 100).toFixed(0)}%`),
      h(
        "td",
        null,
        h(
          "a",
          { href: `#/clips/${m.clip_id}`, class: "link" },
          `clip ${m.clip_id}`,
        ),
      ),
    );
  });
  return h(
    "table",
    { class: "data-table rule-preview-table" },
    h(
      "thead",
      null,
      h(
        "tr",
        null,
        h("th", null, "When"),
        h("th", null, "Camera"),
        h("th", null, "Label"),
        h("th", null, "Conf"),
        h("th", null, "Clip"),
      ),
    ),
    h("tbody", null, ...rows),
  );
}

/// Render the "labels seen in the scanned window" diagnostic that
/// replaces the empty results table when a preview returns zero
/// matches. Three layered hints, most-actionable first:
///
///   1. If the engine echoed back a `effective_when` that differs
///      from the textarea content, the form sent a stale rule —
///      surface that first because it explains every other
///      apparent CEL bug.
///   2. If any rows errored during per-row CEL eval, surface the
///      count + first message — that's almost always the real
///      cause of "should-match-but-doesn't" complaints.
///   3. Otherwise show the label histogram + the COCO namespacing
///      did-you-mean hint (see `nexus-inference/src/yolo.rs::
///      map_coco_to_domain_label` for the mapping table).
function renderScannedLabelsHint(
  labels: ReadonlyArray<{
    label: string;
    count: number;
    matched: number;
    zone_filtered: number;
    label_bytes?: number[];
  }>,
  whenExpr: string,
  evalErrors: number,
  evalFirstError: string | undefined,
  effectiveWhen: string | undefined,
  zoneFilteredTotal: number,
): HTMLElement {
  if (labels.length === 0) {
    return h(
      "div",
      { class: "field-help rule-preview-labels-empty" },
      "No detections in the window — no objects to match against.",
    );
  }

  const children: (HTMLElement | null)[] = [];

  // Hint 1: form sent a different expression than what's typed.
  // We compare normalised whitespace because the engine round-
  // trips the string verbatim but the form may have trimmed
  // trailing newlines; we only flag a real divergence.
  if (
    typeof effectiveWhen === "string" &&
    effectiveWhen.trim() !== whenExpr.trim()
  ) {
    children.push(
      h(
        "div",
        { class: "rule-preview-suggestion rule-preview-suggestion-warn" },
        "The form sent a different expression than what's in the textarea. Switch tabs and back, then re-run preview. Engine evaluated: ",
        h("code", null, effectiveWhen),
      ),
    );
  }

  // Hint 2: per-row CEL eval errors. Almost always the actual
  // cause when label histogram contains the values you expect
  // but matches is still zero.
  if (evalErrors > 0) {
    children.push(
      h(
        "div",
        { class: "rule-preview-suggestion rule-preview-suggestion-warn" },
        `${evalErrors} of the scanned rows errored during CEL evaluation `,
        "(silently skipped by the matcher). First error: ",
        h("code", null, evalFirstError ?? "(unknown)"),
        ". This usually means the predicate references a field ",
        "that isn't populated in preview — e.g. ",
        h("code", null, "object.attributes['motion.dwell_seconds']"),
        " is empty on preview (preview can't reconstruct attributes from a ",
        "single past row). Restructure the rule to gate on stored ",
        "fields only (label, confidence, bbox, camera.id), or test ",
        "against live data instead.",
      ),
    );
  }

  // Hint 2b: the zone gate rejected rows before they even
  // reached the CEL matcher. This is invisible from the
  // predicate alone — the rule looks correct but zones
  // silently filtered out everything. Surfacing the count is
  // crucial because it's the difference between "my CEL is
  // wrong" and "my zones don't cover where this label appears".
  if (zoneFilteredTotal > 0) {
    children.push(
      h(
        "div",
        { class: "rule-preview-suggestion rule-preview-suggestion-warn" },
        `${zoneFilteredTotal} of the scanned rows were rejected by the zone gate `,
        "before the CEL matcher saw them — their bbox-centres fell outside every configured zone. ",
        "Remove the zone filter from the rule (Scope → Zones) and re-run preview to confirm; ",
        "if the matches reappear, the zones don't cover the bboxes for the labels you care about. ",
        "Per-label breakdown appears below.",
      ),
    );
  }

  // Hint 3 (the histogram + did-you-mean) is always useful, even
  // when eval errors are present.
  const families = new Set<string>();
  for (const { label } of labels) {
    const dot = label.indexOf(".");
    if (dot > 0) families.add(label.slice(0, dot));
  }
  const suggestions: string[] = [];
  for (const fam of families) {
    const re = new RegExp(`(['"])${fam}\\1`);
    if (re.test(whenExpr)) {
      suggestions.push(fam);
    }
  }

  const chips = labels.slice(0, 16).map((l) => {
    const zoneSuffix = l.zone_filtered > 0 ? ` (${l.zone_filtered} zone-filtered)` : "";
    return h(
      "span",
      {
        class:
          l.count > 0 && l.matched === 0
            ? "chip rule-preview-label-chip rule-preview-label-chip-zero"
            : "chip rule-preview-label-chip",
        title:
          l.count > 0 && l.matched === 0
            ? `${l.count} rows scanned, 0 matched the rule${zoneSuffix}`
            : `${l.count} rows scanned, ${l.matched} matched the rule${zoneSuffix}`,
      },
      l.label,
      h(
        "span",
        { class: "rule-preview-label-count" },
        ` ${l.matched}/${l.count}${l.zone_filtered > 0 ? ` ⊘${l.zone_filtered}` : ""}`,
      ),
    );
  });

  children.push(
    h(
      "div",
      { class: "field-help" },
      `Detector saw these labels in the window — matched/scanned (top ${chips.length}):`,
    ),
    h("div", { class: "rule-preview-labels" }, ...chips),
  );

  // Byte-level diagnostic: when a label was scanned N times but
  // matched zero, AND no rows were zone-filtered, AND the
  // operator's `when` literally contains that exact label
  // string, the comparison failure is almost always an
  // invisible-character mismatch (NBSP \u00A0, BOM \uFEFF,
  // zero-width-joiner \u200D, smart quote ’ vs '). Dump the
  // bytes of both sides so the operator can see the
  // discrepancy. (When zone_filtered > 0 the explanation is
  // the zone gate above, not the predicate — skip the
  // byte-dump noise.)
  for (const l of labels) {
    if (l.matched !== 0 || l.count === 0) continue;
    if (l.zone_filtered > 0) continue;
    if (l.label_bytes === undefined) continue;
    // Is this label literally mentioned in the operator's CEL?
    // If not, the mismatch is expected (rule doesn't reference
    // this label) — skip the byte dump.
    const re = new RegExp(`(['"])${escapeRegex(l.label)}\\1`);
    if (!re.test(whenExpr)) continue;

    const dbBytes = l.label_bytes;
    const literalMatch = whenExpr.match(
      new RegExp(`(['"])(${escapeRegex(l.label)})\\1`),
    );
    const ruleBytes = literalMatch
      ? Array.from(new TextEncoder().encode(literalMatch[2]!))
      : null;

    const bytesEqual =
      ruleBytes !== null &&
      ruleBytes.length === dbBytes.length &&
      ruleBytes.every((b, i) => b === dbBytes[i]);

    if (!bytesEqual) {
      children.push(
        h(
          "div",
          { class: "rule-preview-suggestion rule-preview-suggestion-warn" },
          `The label `,
          h("code", null, `'${l.label}'`),
          ` was scanned ${l.count} times but matched 0 — and the bytes don't match your rule literal. `,
          "This is almost always a hidden-character mismatch (smart quote, NBSP, BOM, zero-width joiner). Bytes — DB: ",
          h("code", null, `[${dbBytes.join(",")}]`),
          ", rule literal: ",
          h(
            "code",
            null,
            ruleBytes ? `[${ruleBytes.join(",")}]` : "(not found)",
          ),
          ". Try retyping the literal in the textarea.",
        ),
      );
    } else {
      // Bytes match but matcher still says zero — escalate.
      children.push(
        h(
          "div",
          { class: "rule-preview-suggestion rule-preview-suggestion-warn" },
          `The label `,
          h("code", null, `'${l.label}'`),
          ` was scanned ${l.count} times and the bytes match your rule literal exactly, but the matcher returned 0. `,
          "This is a real engine bug — please report it with the rule text and the response payload from the network tab.",
        ),
      );
    }
  }

  if (suggestions.length > 0) {
    for (const fam of suggestions) {
      children.push(
        h(
          "div",
          { class: "rule-preview-suggestion" },
          `Tip: the detector emits namespaced labels like `,
          h("code", null, `${fam}.car`),
          ` — to match every ${fam} variant, use `,
          h("code", null, `object.label.startsWith('${fam}.')`),
          `.`,
        ),
      );
    }
  }

  return h("div", { class: "rule-preview-labels-hint" }, ...children);
}

/// Escape every regex metacharacter so an arbitrary label string
/// can be inlined into a `new RegExp(...)` literal without
/// breaking on `.`, `*`, brackets, etc. Used by the preview byte-
/// dump diagnostic to look up the operator's literal in the CEL.
function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/// "5m ago" / "2h ago" / "3d ago" — falls back to the ISO string
/// for anything older than a week so the operator still gets
/// absolute context for stale rows.
function formatRelativeTime(iso: string): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return iso;
  const deltaMs = Date.now() - t;
  const s = Math.max(0, Math.floor(deltaMs / 1000));
  if (s < 60) return `${s}s ago`;
  const mins = Math.floor(s / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;
  // Older than a week — show ISO date only.
  return iso.slice(0, 10);
}

/// Snapshot of the current FormState into a transient RuleConfig
/// suitable for the preview endpoint. Mirrors `toRuleConfig` but
/// tolerates partial state (empty id / name) — the engine doesn't
/// persist the preview, so blank fields are fine.
function toPreviewRule(state: FormState): RuleConfig {
  return {
    id: state.id.trim() || "__preview__",
    name: state.name.trim() || "preview",
    severity: state.severity,
    camera_filter:
      state.camera_filter.length === 0 ? null : [...state.camera_filter],
    zones: state.zones.length === 0 ? null : [...state.zones],
    when: state.when.trim(),
    min_track_age_ms: state.min_track_age_ms,
    consecutive_frames: state.consecutive_frames,
    cooldown_ms: state.cooldown_ms,
    enabled: state.enabled,
  };
}
