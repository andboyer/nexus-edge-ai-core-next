// M2.2 Phase 5 — Storage admin tab.
// M-Admin Phase 6 polish — native browser `alert()` / `confirm()`
// modals replaced by the shared `toast` + `openDialog` primitives
// so the destructive-delete UX matches Cameras / Rules. Inline
// `errorBanner` paragraphs are kept (the operator needs them to
// stay put while correcting the form); success now also fires a
// `toast.success` so the action is acknowledged outside the form.
//
// Operator surface for the cold-replication policy. Three sections:
//
//   1. Hot tier — read-only mirror of the engine's hot-side state
//      (recorder kind, watermark FSM snapshot, free %, clips_dir).
//      Mirrors what the Storage tab shows; included here so the
//      operator has one screen for all of "where do my clips live"
//      without tab-hopping. USB tiering surfaces the live attached
//      volumes plus an editor for `preferred_usb_label` that PUTs
//      `/v1/admin/runtime/usb_preferred` (no restart required).
//
//   2. Cold backends — table of every row in `storage_backends`
//      (the implicit `'local'` backend is omitted from delete UX
//      since the engine rejects that). Per-row controls:
//         * Make active: PUT /v1/admin/storage/cold {handle}
//         * Delete:      DELETE /v1/admin/storage/backends/{handle}
//      Plus an "Add backend" form below the table. v1 supports the
//      `lan` kind plus the M2.2 closeout cloud kinds (`gdrive`,
//      `onedrive`). Cloud kinds drive the auth-code flow directly
//      in this binary — see `runOAuthFlow` below — so the operator
//      never copy-pastes a refresh token.
//
//   3. Cold replication policy — the singleton row in
//      `storage_cold_replica`. Carries the active handle (chosen
//      from section 2), throttle (slider in MB/s, 0 means
//      unthrottled), and the live counter card.
//
// Out of scope for this PR (deferred):
//   * Per-backend test-write button (existing `health()` probe is
//      good enough for v1)

import { api } from "../api/client.js";
import { clear, h } from "../lib/el.js";
import { openDialog, dialogFooter, type DialogHandle } from "../lib/dialog.js";
import { toast } from "../lib/toast.js";
import { iconButton } from "../lib/icons.js";
import type {
  ColdHealthOut,
  ColdStatus,
  OAuthStartReq,
  OAuthStatusResp,
  StorageBackendOut,
  StorageHotSection,
  StorageResponse,
  UsbSection,
} from "../api/types.js";

const COLD_KINDS: { id: string; label: string; enabled: boolean; hint: string }[] = [
  { id: "lan", label: "LAN share", enabled: true, hint: "Mounted directory on the local network." },
  {
    id: "gdrive",
    label: "Google Drive",
    enabled: true,
    hint:
      "OAuth-backed cold tier (Drive AppFolder). Connect button launches the consent flow in this engine — no copy-pasted refresh tokens.",
  },
  {
    id: "onedrive",
    label: "OneDrive",
    enabled: true,
    hint:
      "OAuth-backed cold tier (Microsoft Graph / AppFolder). Connect button launches the consent flow in this engine — no copy-pasted refresh tokens.",
  },
];

export async function renderAdminStorage(root: HTMLElement): Promise<void> {
  clear(root);
  root.append(h("h2", null, "Storage Admin"));

  const status = h("p", { class: "muted" }, "Loading storage state…");
  root.append(status);

  const hotHost = h("section", { class: "admin-section" });
  const backendsHost = h("section", { class: "admin-section" });
  const policyHost = h("section", { class: "admin-section" });
  root.append(hotHost, backendsHost, policyHost);

  const reload = async (): Promise<void> => {
    let body: StorageResponse;
    try {
      body = await api.storage.full();
    } catch (e) {
      status.textContent = `Storage state unavailable — ${(e as Error).message}`;
      return;
    }
    status.textContent = "";
    renderHotSection(hotHost, body.hot, body.usb);
    renderBackendsSection(backendsHost, body.backends, body.cold, reload);
    renderPolicySection(policyHost, body.cold, body.backends, reload);
  };

  // The USB-preferred editor lives inside the pure `renderHotSection`
  // renderer (no reload closure in scope), so it announces successful
  // PUTs via a bubbling custom event that we catch here to refetch.
  root.addEventListener("nexus:storage-reload", () => {
    void reload();
  });

  await reload();
}

// ---- Hot section ---------------------------------------------------------

function renderHotSection(
  host: HTMLElement,
  hot: StorageHotSection,
  usb: UsbSection,
): void {
  clear(host);
  host.append(h("h3", null, "Hot tier (local NVR)"));

  const tone =
    hot.watermark_state === "panic"
      ? "panic"
      : hot.watermark_state === "low"
        ? "warn"
        : "ok";
  const dotTone = tone === "ok" ? "ok" : tone === "warn" ? "warn" : "crit";

  host.append(
    h(
      "div",
      { class: `storage-card ${tone}` },
      h(
        "div",
        { class: "storage-card-head" },
        h("span", { class: `dot dot-${dotTone}` }),
        h("strong", null, "Recorder"),
        h("span", { class: "muted" }, ` · kind = `),
        h("code", null, hot.recorder_kind),
        h("span", { class: "muted" }, ` · watermark = `),
        h("code", null, hot.watermark_state),
        hot.panic ? h("span", { class: "panic-pill" }, "PANIC") : null,
      ),
      h(
        "div",
        { class: "storage-card-line" },
        h(
          "span",
          { class: "metric" },
          h("span", { class: "k" }, "Free"),
          h("span", null, hot.free_pct != null ? `${hot.free_pct.toFixed(1)}%` : "—"),
        ),
        h(
          "span",
          { class: "metric" },
          h("span", { class: "k" }, "Disk"),
          h(
            "span",
            null,
            hot.fs_total_bytes != null && hot.fs_used_bytes != null
              ? `${formatBytes(hot.fs_used_bytes)} / ${formatBytes(hot.fs_total_bytes)}`
              : "—",
          ),
        ),
        h(
          "span",
          { class: "metric" },
          h("span", { class: "k" }, "Path"),
          h("code", null, hot.clips_dir),
        ),
        h(
          "span",
          { class: "metric muted" },
          h("span", { class: "k" }, "Thresholds"),
          h(
            "span",
            null,
            `low ${hot.watermark_low_pct}% · panic ${hot.watermark_panic_pct}%`,
          ),
        ),
      ),
    ),
  );

  // M2.2 Phase 3 — USB hot-plug. Lists volumes the engine's
  // `usb_watch` task currently sees under `<clips_dir>/usb/`. The
  // configured `preferred_usb_label` is highlighted; an unmounted
  // preferred shows amber so the operator knows the recorder will
  // silently fall back to the local hot tier on the next clip.
  host.append(renderUsbCard(usb));
}

function renderUsbCard(usb: UsbSection): HTMLElement {
  const headDot =
    usb.preferred_label && !usb.preferred_active
      ? "warn"
      : usb.attached.length > 0
        ? "ok"
        : "ok";
  const headText = usb.preferred_label
    ? usb.preferred_active
      ? `routing to "${usb.preferred_label}"`
      : `preferred "${usb.preferred_label}" not attached → falling back to local`
    : `${usb.attached.length} attached · no preferred label set`;

  const rows: HTMLElement[] =
    usb.attached.length === 0
      ? [
          h(
            "div",
            { class: "storage-card-line muted" },
            h(
              "span",
              null,
              "No NEXUS_*-labeled USB volumes detected. Plug a labeled stick in or symlink ",
            ),
            h("code", null, "/Volumes"),
            h("span", null, " into "),
            h("code", null, "<clips_dir>/usb"),
            h("span", null, " on macOS dev."),
          ),
        ]
      : usb.attached.map((v) => {
          const isPreferred = usb.preferred_label === v.label;
          const tone = isPreferred ? (usb.preferred_active ? "ok" : "warn") : undefined;
          return h(
            "div",
            { class: "storage-card-line" },
            h("span", { class: `dot dot-${tone ?? "ok"}` }),
            h("strong", null, v.label),
            isPreferred
              ? h(
                  "span",
                  { class: "muted" },
                  ` · preferred${usb.preferred_active ? " (active)" : ""}`,
                )
              : null,
            h("span", { class: "metric muted" }, h("span", { class: "k" }, "Mount")),
            h("code", null, v.mount_relpath),
          );
        });

  return h(
    "div",
    { class: "storage-card" },
    h(
      "div",
      { class: "storage-card-head" },
      h("span", { class: `dot dot-${headDot}` }),
      h("strong", null, "USB tiering"),
      h("span", { class: "muted" }, ` · ${headText}`),
    ),
    ...rows,
    renderUsbPreferredEditor(usb),
  );
}

// M2.2 closeout — live preferred-USB-label editor. PUTs
// `/v1/admin/runtime/usb_preferred` and the engine persists the
// choice in `engine_runtime_settings` AND flips the shared
// `PreferredUsbLabel` handle so the next clip honours it without
// restart.
function renderUsbPreferredEditor(usb: UsbSection): HTMLElement {
  const wrap = h("div", { class: "storage-card-line usb-preferred-editor" });

  const labelLine = h(
    "span",
    null,
    h("span", { class: "k" }, "Preferred label"),
    h("span", { class: "muted" }, " — routes new clips to this volume; in-flight clips finish where they started"),
  );

  // Build the picker. We want both:
  //   (a) a dropdown sourced from currently-attached volumes (so the
  //       common case is one click), AND
  //   (b) a free-text fallback so the operator can pre-configure a
  //       label whose volume isn't plugged in yet.
  const select = h("select", { name: "usb_preferred_select" }) as HTMLSelectElement;
  select.append(h("option", { value: "" }, "(Clear — fall back to local)"));
  const attachedLabels = usb.attached.map((v) => v.label);
  const knownLabels = new Set<string>(attachedLabels);
  if (usb.preferred_label != null) {
    knownLabels.add(usb.preferred_label);
  }
  for (const label of Array.from(knownLabels).sort()) {
    select.append(
      h(
        "option",
        {
          value: label,
          selected: usb.preferred_label === label,
        },
        attachedLabels.includes(label) ? label : `${label} (not attached)`,
      ),
    );
  }
  // Free-text "other" option that reveals the text input below.
  select.append(
    h(
      "option",
      { value: "__other__" },
      "Other / not in this list…",
    ),
  );

  const textInput = h("input", {
    type: "text",
    name: "usb_preferred_text",
    placeholder: "e.g. NEXUS_VAULT_42",
    style: { display: "none" },
  }) as HTMLInputElement;

  select.addEventListener("change", () => {
    if (select.value === "__other__") {
      textInput.style.display = "";
      textInput.focus();
    } else {
      textInput.style.display = "none";
      textInput.value = "";
    }
  });

  const saveBtn = h(
    "button",
    {
      type: "button",
      class: "ghost",
      title: "PUT /v1/admin/runtime/usb_preferred — persists the choice and updates the live handle in one go.",
    },
    "Save",
  );
  const clearBtn = h(
    "button",
    {
      type: "button",
      class: "ghost",
      title: "PUT /v1/admin/runtime/usb_preferred with {label: null} — clears the preference so new clips go to local.",
    },
    "Clear",
  );
  const banner = h("span", { class: "muted" }, "");

  const applyLabel = (label: string | null): void => {
    saveBtn.setAttribute("disabled", "true");
    clearBtn.setAttribute("disabled", "true");
    banner.textContent = "Saving…";
    void api.adminStorage
      .usbPreferred(label)
      .then(() => {
        banner.textContent = label == null ? "Preference cleared." : `Preferred = "${label}"`;
        // Trigger an outer reload via a custom event the page
        // listens for; the renderUsbCard caller doesn't get a
        // reload closure (it's a pure renderer), so we dispatch
        // an event the section host picks up.
        wrap.dispatchEvent(
          new CustomEvent("nexus:storage-reload", { bubbles: true }),
        );
      })
      .catch((e: Error) => {
        banner.textContent = `Failed: ${e.message}`;
      })
      .finally(() => {
        saveBtn.removeAttribute("disabled");
        clearBtn.removeAttribute("disabled");
      });
  };

  saveBtn.addEventListener("click", () => {
    let chosen: string | null;
    if (select.value === "__other__") {
      const trimmed = textInput.value.trim();
      if (trimmed === "") {
        banner.textContent = "Enter a label or pick Clear.";
        return;
      }
      chosen = trimmed;
    } else if (select.value === "") {
      chosen = null;
    } else {
      chosen = select.value;
    }
    applyLabel(chosen);
  });
  clearBtn.addEventListener("click", () => {
    select.value = "";
    textInput.style.display = "none";
    textInput.value = "";
    applyLabel(null);
  });

  wrap.append(labelLine, select, textInput, saveBtn, clearBtn, banner);
  return wrap;
}

// ---- Backends section ----------------------------------------------------

function renderBackendsSection(
  host: HTMLElement,
  backends: StorageBackendOut[],
  active: ColdStatus | null,
  reload: () => Promise<void>,
): void {
  clear(host);
  host.append(h("h3", null, "Cold backends"));
  host.append(
    h(
      "p",
      { class: "muted" },
      "Each row in the engine's storage_backends table. The implicit ",
      h("code", null, "local"),
      " backend is shown for context but cannot be deleted or used as a cold target.",
    ),
  );

  // Table.
  const table = h("table", { class: "admin-table" });
  table.append(
    h(
      "thead",
      null,
      h(
        "tr",
        null,
        h("th", null, "Handle"),
        h("th", null, "Kind"),
        h("th", null, "Config"),
        h("th", null, "Updated"),
        h("th", null, "Active cold"),
        h("th", null, "Actions"),
      ),
    ),
  );
  const tbody = h("tbody", null);
  if (backends.length === 0) {
    tbody.append(
      h(
        "tr",
        null,
        h(
          "td",
          { colSpan: 6, class: "muted" } as never,
          "No backends registered. Add one below to enable cold replication.",
        ),
      ),
    );
  }
  for (const b of backends) {
    const isActive = active != null && active.handle === b.handle;
    const isLocal = b.handle === "local";
    const actions = h("td", null);
    if (!isLocal) {
      actions.append(
        h(
          "button",
          {
            class: "ghost",
            disabled: isActive,
            on: {
              click: () => void promoteBackend(b.handle, reload),
            },
            title: isActive
              ? "Already the active cold backend."
              : "PUT /v1/admin/storage/cold to point cold replication here.",
          },
          isActive ? "Active" : "Make active",
        ),
        iconButton("trash", {
          title:
            "DELETE /v1/admin/storage/backends/:handle. Fails if any motion_clips row references this backend.",
          onClick: () => void deleteBackend(b.handle, reload),
        }),
      );
    } else {
      actions.append(h("span", { class: "muted" }, "—"));
    }
    tbody.append(
      h(
        "tr",
        null,
        h("td", null, h("code", null, b.handle)),
        h("td", null, b.kind),
        h(
          "td",
          null,
          h("code", { class: "muted mono" }, JSON.stringify(b.config)),
        ),
        h("td", { class: "muted" }, formatTs(b.updated_at)),
        h("td", null, isActive ? h("span", { class: "health-pill health-ok" }, "Yes") : h("span", { class: "muted" }, "—")),
        actions,
      ),
    );
  }
  table.append(tbody);
  host.append(table);

  // Add-backend form.
  host.append(renderAddBackendForm(reload));
}

function renderAddBackendForm(reload: () => Promise<void>): HTMLElement {
  const form = h("form", { class: "admin-form" });
  form.append(h("h4", null, "Register a new backend"));

  const handleInput = h("input", {
    type: "text",
    name: "handle",
    placeholder: "e.g. nas-archive",
    required: true,
    pattern: "^[a-z0-9][a-z0-9_-]*$",
    title: "Lowercase letters, digits, underscore or dash. Must start with a letter or digit.",
  }) as HTMLInputElement;

  // Kind radio group. Only `lan` is enabled in v1; cloud kinds are
  // surfaced as disabled radios so operators can see the roadmap.
  const kindGroup = h("div", { class: "radio-group" });
  let selectedKind = "lan";
  const kindRadios: HTMLInputElement[] = [];
  for (const k of COLD_KINDS) {
    const radio = h("input", {
      type: "radio",
      name: "kind",
      value: k.id,
      disabled: !k.enabled,
      checked: k.id === "lan",
    }) as HTMLInputElement;
    radio.addEventListener("change", () => {
      if (radio.checked) {
        selectedKind = radio.value;
        renderConfigBody();
      }
    });
    kindRadios.push(radio);
    kindGroup.append(
      h(
        "label",
        {
          class: k.enabled ? "" : "muted",
          title: k.hint,
        },
        radio,
        h("span", null, ` ${k.label}`),
        k.enabled ? null : h("span", { class: "muted" }, " (Phase 2)"),
      ),
    );
  }

  // Per-kind config body. Re-rendered on radio change. LAN emits a
  // single-field form; cloud kinds emit the OAuth-driven Connect
  // form (client_id/secret + account email + optional root folder;
  // the refresh token is minted server-side by the auth-code flow,
  // never typed into a textbox).
  const configHost = h("div", { class: "config-host" });
  const renderConfigBody = (): void => {
    clear(configHost);
    if (selectedKind === "lan") {
      configHost.append(
        h(
          "label",
          null,
          h("span", null, "Root path "),
          h("input", {
            type: "text",
            name: "lan_root",
            placeholder: "/mnt/nas-archive",
            required: true,
          }),
        ),
        h(
          "p",
          { class: "muted" },
          "Path on the engine host where clips will be written. The directory must exist and be writable by the engine user.",
        ),
      );
    } else if (selectedKind === "gdrive" || selectedKind === "onedrive") {
      const isGdrive = selectedKind === "gdrive";
      const providerLabel = isGdrive ? "Google Drive" : "OneDrive";
      const scopeText = isGdrive
        ? "drive.file (AppFolder-scoped)"
        : "Files.ReadWrite.AppFolder + offline_access";
      configHost.append(
        h(
          "div",
          { class: "muted oauth-banner" },
          h("strong", null, "Connect flow. "),
          h(
            "span",
            null,
            `Clicking Connect opens ${providerLabel} in a popup for consent on scope `,
          ),
          h("code", null, scopeText),
          h(
            "span",
            null,
            ". The engine receives the redirect at /api/v1/admin/oauth/" +
              `${selectedKind}/callback`,
          ),
          h(
            "span",
            null,
            `, exchanges the auth code for a refresh + access token pair, encrypts the refresh token with the admin secret (AES-256-GCM, HKDF-derived subkey), and upserts the backend row. The refresh token never reaches this browser.`,
          ),
        ),
        h(
          "label",
          null,
          h("span", null, "Client ID "),
          h("input", {
            type: "text",
            name: "client_id",
            placeholder: isGdrive
              ? "xxxxxxxxxxxx.apps.googleusercontent.com"
              : "00000000-0000-0000-0000-000000000000",
            required: true,
            autocomplete: "off",
          }),
        ),
        h(
          "label",
          null,
          h("span", null, "Client secret "),
          h("input", {
            type: "password",
            name: "client_secret",
            placeholder: "(from the OAuth app registration)",
            required: true,
            autocomplete: "new-password",
          }),
        ),
        h(
          "label",
          null,
          h("span", null, "Account email "),
          h("input", {
            type: "email",
            name: "account_email",
            placeholder: "ops@example.com",
            required: true,
          }),
        ),
        h(
          "p",
          { class: "muted" },
          `Pre-register ${providerLabel} OAuth credentials with this engine's callback URL: ${window.location.origin}/api/v1/admin/oauth/${selectedKind}/callback`,
        ),
      );
      if (isGdrive) {
        configHost.append(
          h(
            "label",
            null,
            h("span", null, "Root folder ID (optional) "),
            h("input", {
              type: "text",
              name: "root_folder_id",
              placeholder: "(blank = drive root; e.g. 1AbCd…XyZ)",
            }),
          ),
        );
      }
    } else {
      configHost.append(
        h(
          "p",
          { class: "muted" },
          `Backend kind '${selectedKind}' is not supported in this build.`,
        ),
      );
    }
  };
  renderConfigBody();

  // Submit button label tracks the selected kind so the cloud
  // variants show "Connect Google Drive" / "Connect OneDrive"
  // instead of "Register backend" \u2014 the actual side effect (popup
  // + status polling) is also kind-aware.
  const submitBtn = h(
    "button",
    { type: "submit", class: "primary" },
    "Register backend",
  ) as HTMLButtonElement;
  const updateSubmitLabel = (): void => {
    if (selectedKind === "gdrive") {
      submitBtn.textContent = "Connect Google Drive";
    } else if (selectedKind === "onedrive") {
      submitBtn.textContent = "Connect OneDrive";
    } else {
      submitBtn.textContent = "Register backend";
    }
  };
  // Hook into the existing radio listeners by re-installing them:
  for (const radio of kindRadios) {
    radio.addEventListener("change", () => {
      if (radio.checked) {
        updateSubmitLabel();
      }
    });
  }
  updateSubmitLabel();

  const errorBanner = h("p", { class: "muted" }, "");

  form.append(
    h("label", null, h("span", null, "Handle "), handleInput),
    h("div", { class: "form-row" }, h("span", null, "Kind "), kindGroup),
    configHost,
    h("div", { class: "form-actions" }, submitBtn),
    errorBanner,
  );

  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    errorBanner.textContent = "";
    const handle = handleInput.value.trim();
    if (handle === "") {
      errorBanner.textContent = "Handle is required.";
      return;
    }
    if (selectedKind === "lan") {
      const rootInput = form.querySelector("input[name=lan_root]") as HTMLInputElement | null;
      const root = rootInput?.value.trim() ?? "";
      if (root === "") {
        errorBanner.textContent = "LAN root path is required.";
        return;
      }
      const config = { root };
      submitBtn.setAttribute("disabled", "true");
      submitBtn.textContent = "Saving\u2026";
      void api.adminStorage
        .upsertBackend(handle, { kind: "lan", config })
        .then(async () => {
          handleInput.value = "";
          const el = form.querySelector("input[name=lan_root]") as HTMLInputElement | null;
          if (el) el.value = "";
          await reload();
        })
        .catch((e: Error) => {
          errorBanner.textContent = `Failed: ${e.message}`;
        })
        .finally(() => {
          submitBtn.removeAttribute("disabled");
          updateSubmitLabel();
        });
      return;
    }
    if (selectedKind === "gdrive" || selectedKind === "onedrive") {
      const cidInput = form.querySelector("input[name=client_id]") as HTMLInputElement | null;
      const csInput = form.querySelector("input[name=client_secret]") as HTMLInputElement | null;
      const emInput = form.querySelector("input[name=account_email]") as HTMLInputElement | null;
      const clientId = cidInput?.value.trim() ?? "";
      const clientSecret = csInput?.value ?? "";
      const accountEmail = emInput?.value.trim() ?? "";
      if (clientId === "" || clientSecret === "" || accountEmail === "") {
        errorBanner.textContent =
          "Client ID, secret, and account email are all required.";
        return;
      }
      const rfInput = form.querySelector("input[name=root_folder_id]") as HTMLInputElement | null;
      const rootFolderId =
        selectedKind === "gdrive" ? (rfInput?.value.trim() ?? "") : "";
      const redirectUri = `${window.location.origin}/api/v1/admin/oauth/${selectedKind}/callback`;
      const startReq: OAuthStartReq = {
        handle,
        client_id: clientId,
        client_secret: clientSecret,
        account_email: accountEmail,
        root_folder_id: rootFolderId === "" ? null : rootFolderId,
        redirect_uri: redirectUri,
      };
      submitBtn.setAttribute("disabled", "true");
      submitBtn.textContent = "Connecting\u2026";
      void runOAuthFlow(selectedKind, startReq, errorBanner)
        .then(async (completedHandle) => {
          // Clear every cleartext field on success so the operator
          // doesn't leave secrets sitting in the form.
          handleInput.value = "";
          for (const name of [
            "client_id",
            "client_secret",
            "account_email",
            "root_folder_id",
          ]) {
            const el = form.querySelector(`input[name=${name}]`) as HTMLInputElement | null;
            if (el) el.value = "";
          }
          errorBanner.textContent = `Connected. Registered '${completedHandle}'.`;
          await reload();
        })
        .catch((e: Error) => {
          errorBanner.textContent = `Connect failed: ${e.message}`;
        })
        .finally(() => {
          submitBtn.removeAttribute("disabled");
          updateSubmitLabel();
        });
      return;
    }
    errorBanner.textContent = `Backend kind '${selectedKind}' is not supported in this build.`;
  });

  return form;
}

/// Run the OAuth 3-leg flow end-to-end from the operator's browser:
///
///   1. POST /api/v1/admin/oauth/{provider}/start with the form
///      payload — engine returns `{ authorize_url, state,
///      expires_in_secs }` and stashes the pending session in
///      memory keyed by `state`.
///   2. window.open(authorize_url) into a popup so the consent flow
///      stays out of the main admin tab.
///   3. Poll GET /api/v1/admin/oauth/status?state= every 2 s until
///      the engine reports `complete` (callback fired, refresh
///      token encrypted + upserted) or `error` (provider rejected
///      consent, or the operator closed the popup before approving).
///
/// Returns the backend handle the engine ultimately upserted.
async function runOAuthFlow(
  provider: string,
  body: OAuthStartReq,
  errorBanner: HTMLElement,
): Promise<string> {
  errorBanner.textContent = "Opening provider consent…";
  const startResp = await api.adminStorage.oauthStart(provider, body);
  const popup = window.open(
    startResp.authorize_url,
    `nexus-oauth-${provider}`,
    "popup=yes,width=600,height=720,noopener=no",
  );
  if (!popup) {
    throw new Error(
      "Popup blocked. Allow popups for this engine and click Connect again.",
    );
  }
  errorBanner.textContent = "Waiting for consent…";
  const deadline = Date.now() + startResp.expires_in_secs * 1000;
  const POLL_MS = 2000;
  // First poll happens after the popup has had time to redirect.
  // We *don't* require the popup to stay open after consent: the
  // engine's callback runs server-side and the popup can close
  // itself via the callback's HTML response.
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, POLL_MS));
    let resp: OAuthStatusResp;
    try {
      resp = await api.adminStorage.oauthStatus(startResp.state);
    } catch (e) {
      // 404 after a successful completion is fine — the engine
      // sweeps completed sessions. Surface anything else.
      const msg = (e as Error).message;
      if (msg.startsWith("404")) {
        throw new Error("Session expired before consent completed.");
      }
      throw e;
    }
    if (resp.status === "complete") {
      try {
        popup.close();
      } catch {
        /* popup may already be closed by callback HTML */
      }
      return resp.handle;
    }
    if (resp.status === "error") {
      try {
        popup.close();
      } catch {
        /* same */
      }
      throw new Error(resp.message);
    }
    // status === "pending" — keep polling.
  }
  try {
    popup.close();
  } catch {
    /* popup may already be closed */
  }
  throw new Error("Timed out waiting for consent.");
}

async function promoteBackend(
  handle: string,
  reload: () => Promise<void>,
): Promise<void> {
  try {
    await api.adminStorage.cold({ handle });
    toast.success(`Cold replication now targets '${handle}'.`);
    await reload();
  } catch (e) {
    toast.error(`Failed to make '${handle}' active: ${(e as Error).message}`);
  }
}

async function deleteBackend(
  handle: string,
  reload: () => Promise<void>,
): Promise<void> {
  const confirmed = await confirmDeleteBackend(handle);
  if (!confirmed) return;
  try {
    await api.adminStorage.removeBackend(handle);
    toast.success(`Backend '${handle}' deleted.`);
    await reload();
  } catch (e) {
    toast.error(`Delete failed: ${(e as Error).message}`);
  }
}

/// Shared confirmation dialog for the destructive backend-delete
/// action. Matches the cameras.ts / rules.ts pattern (danger-toned
/// footer, single-paragraph body explaining the precondition the
/// engine enforces). Resolves true on confirm, false on cancel /
/// dismiss.
function confirmDeleteBackend(handle: string): Promise<boolean> {
  const body = h(
    "p",
    null,
    "Delete cold backend ",
    h("strong", null, h("code", null, handle)),
    "? The engine refuses this if any motion_clips row still references the backend \u2014 you'll get an inline error if so.",
  );
  let dlg: DialogHandle | null = null;
  const footer = dialogFooter({
    cancelLabel: "Cancel",
    confirmLabel: "Delete",
    confirmTone: "danger",
    onCancel: () => dlg?.close(false),
    onConfirm: () => dlg?.close(true),
  });
  dlg = openDialog({
    title: "Delete cold backend",
    body,
    footer,
    width: "460px",
  });
  return dlg.closed;
}

// ---- Policy section ------------------------------------------------------

function renderPolicySection(
  host: HTMLElement,
  cold: ColdStatus | null,
  backends: StorageBackendOut[],
  reload: () => Promise<void>,
): void {
  clear(host);
  host.append(h("h3", null, "Cold replication policy"));

  // Backends eligible to be the cold target: anything except the
  // implicit `'local'` (which is hot). The select includes a "Disabled"
  // option that maps to `handle: null`.
  const coldEligible = backends.filter((b) => b.handle !== "local");

  if (coldEligible.length === 0 && cold == null) {
    host.append(
      h(
        "p",
        { class: "muted" },
        "Register a backend above before enabling cold replication.",
      ),
    );
    return;
  }

  // Live counter card. Always render: when cold is null, show zeros
  // so the operator can see the schema of what they'll be turning on.
  host.append(renderColdStatsCard(cold));

  // Editor.
  const form = h("form", { class: "admin-form" });
  const select = h("select", { name: "cold_handle" }) as HTMLSelectElement;
  select.append(
    h("option", { value: "" }, "(Disabled — no cold replication)"),
  );
  for (const b of coldEligible) {
    select.append(
      h(
        "option",
        { value: b.handle, selected: cold != null && cold.handle === b.handle },
        `${b.handle} (${b.kind})`,
      ),
    );
  }

  // Throttle slider. Range: 0 (unthrottled) to 524_288_000 (500 MB/s)
  // on a log scale so the lower end is editable. We expose two
  // inputs: a slider plus a numeric MB/s box that mirror each other.
  const initialBps = cold?.throttle_bps ?? 0;
  const slider = h("input", {
    type: "range",
    min: "0",
    max: "1000",
    value: String(bpsToSlider(initialBps)),
    name: "throttle_slider",
  }) as HTMLInputElement;
  const mbsBox = h("input", {
    type: "number",
    min: "0",
    step: "any",
    value: bpsToMbs(initialBps),
    name: "throttle_mbs",
  }) as HTMLInputElement;
  const throttleLabel = h("span", { class: "muted" }, formatThrottle(initialBps));

  slider.addEventListener("input", () => {
    const bps = sliderToBps(Number(slider.value));
    mbsBox.value = bpsToMbs(bps);
    throttleLabel.textContent = formatThrottle(bps);
  });
  mbsBox.addEventListener("input", () => {
    const bps = mbsToBps(Number(mbsBox.value));
    slider.value = String(bpsToSlider(bps));
    throttleLabel.textContent = formatThrottle(bps);
  });

  const submitBtn = h(
    "button",
    { type: "submit", class: "primary" },
    "Save policy",
  );
  const errorBanner = h("p", { class: "muted" }, "");

  form.append(
    h(
      "label",
      null,
      h("span", null, "Active backend "),
      select,
    ),
    h(
      "div",
      { class: "form-row" },
      h("span", null, "Throttle "),
      slider,
      h("span", null, " "),
      mbsBox,
      h("span", null, " MB/s "),
      throttleLabel,
    ),
    h(
      "p",
      { class: "muted" },
      "0 means unthrottled. The replicator runs a token-bucket against this rate; the actual upload speed is min(throttle, backend speed).",
    ),
    h("div", { class: "form-actions" }, submitBtn),
    errorBanner,
  );

  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    errorBanner.textContent = "";
    const handle = select.value === "" ? null : select.value;
    const bps = mbsToBps(Number(mbsBox.value));
    submitBtn.setAttribute("disabled", "true");
    submitBtn.textContent = "Saving…";
    void api.adminStorage
      .cold({ handle, throttle_bps: bps })
      .then(reload)
      .catch((e: Error) => {
        errorBanner.textContent = `Failed: ${e.message}`;
      })
      .finally(() => {
        submitBtn.removeAttribute("disabled");
        submitBtn.textContent = "Save policy";
      });
  });

  host.append(form);
}

function renderColdStatsCard(cold: ColdStatus | null): HTMLElement {
  const tone = cold == null ? "ok" : coldHealthTone(cold.health);
  const dotTone = tone === "ok" ? "ok" : tone === "warn" ? "warn" : "crit";
  const head = h(
    "div",
    { class: "storage-card-head" },
    h("span", { class: `dot dot-${dotTone}` }),
    h("strong", null, "Replication status"),
    cold == null
      ? h("span", { class: "muted" }, " · disabled")
      : h(
          "span",
          { class: "muted" },
          ` · backend `,
          h("code", null, cold.handle),
          ` (${cold.kind})`,
        ),
    cold != null
      ? h("span", { class: `health-pill health-${tone}` }, coldHealthLabel(cold.health))
      : null,
  );
  const c = cold;
  const metrics = h(
    "div",
    { class: "storage-card-line" },
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Pending"),
      h("span", null, String(c?.pending_count ?? 0)),
    ),
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Replicated"),
      h("span", null, String(c?.replicated_count ?? 0)),
    ),
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Cold-only"),
      h("span", null, String(c?.cold_only_count ?? 0)),
    ),
    h(
      "span",
      { class: "metric" },
      h("span", { class: "k" }, "Lifetime uploaded"),
      h("span", null, formatBytes(c?.lifetime_uploaded_bytes ?? 0)),
    ),
  );
  return h(
    "div",
    { class: `storage-card ${tone === "ok" ? "" : tone === "warn" ? "warn" : "panic"}` },
    head,
    metrics,
  );
}

// ---- Helpers -------------------------------------------------------------

function coldHealthTone(h: ColdHealthOut): "ok" | "warn" | "crit" {
  switch (h.status) {
    case "ok":
      return "ok";
    case "read_only":
      return "warn";
    case "unreachable":
    case "not_registered":
      return "crit";
  }
}

function coldHealthLabel(h: ColdHealthOut): string {
  switch (h.status) {
    case "ok":
      return "Ok";
    case "read_only":
      return "Read-only";
    case "unreachable":
      return "Unreachable";
    case "not_registered":
      return "Not registered";
  }
}

// Slider <-> bps mapping. We use a log-ish curve so the bottom of
// the range (0..1 MB/s) is selectable. Slider ints map linearly to
// log10(bps + 1) over [0, log10(500_000_000 + 1)].
const MAX_BPS = 524_288_000; // 500 MB/s ceiling.
function bpsToSlider(bps: number): number {
  if (bps <= 0) return 0;
  const ratio = Math.log10(bps + 1) / Math.log10(MAX_BPS + 1);
  return Math.round(Math.min(1, Math.max(0, ratio)) * 1000);
}
function sliderToBps(slider: number): number {
  if (slider <= 0) return 0;
  const ratio = slider / 1000;
  const bps = Math.pow(10, ratio * Math.log10(MAX_BPS + 1)) - 1;
  return Math.round(Math.max(0, Math.min(MAX_BPS, bps)));
}
function bpsToMbs(bps: number): string {
  if (bps <= 0) return "0";
  const mbs = bps / 1_048_576;
  return mbs >= 10 ? mbs.toFixed(1) : mbs.toFixed(3);
}
function mbsToBps(mbs: number): number {
  if (!Number.isFinite(mbs) || mbs <= 0) return 0;
  return Math.min(MAX_BPS, Math.round(mbs * 1_048_576));
}
function formatThrottle(bps: number): string {
  if (bps <= 0) return "(unthrottled)";
  if (bps >= 1_048_576) return `≈ ${(bps / 1_048_576).toFixed(1)} MB/s`;
  if (bps >= 1024) return `≈ ${(bps / 1024).toFixed(1)} kB/s`;
  return `${bps} B/s`;
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)} ${units[i]}`;
}

function formatTs(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}
