// M6 Phase 2 Step 2.9 — full-screen login overlay.
//
// Rendered into a dedicated `#auth-overlay` host outside the
// app grid when `auth.mode in {local, hybrid}` AND there's no
// valid session. Self-removes once login succeeds AND any
// follow-up `force_password_reset` modal resolves.
//
// Posture choices, deliberate:
//
// * The overlay is the only DOM surface visible until login
//   finishes. We do NOT render the sidebar/main shell "behind"
//   the form (no peek-through, no clickjacking surface).
// * Generic error text. The engine returns the same
//   `invalid_credentials` shape for unknown-user, wrong-password,
//   locked, and disabled — we mirror that opacity in the UI.
// * Submit button stays disabled while the request is in
//   flight to prevent the double-submit race that would burn
//   two lockout slots.

import { auth as authApi } from "../api/auth.js";
import {
  getSession,
  sessionFromTokenResponse,
  setSession,
} from "../lib/auth.js";
import { mountForcePasswordResetModal } from "./change-password-modal.js";

const HOST_ID = "auth-overlay";

/// Mount the login overlay. Idempotent — if already mounted,
/// re-uses the existing host. `onComplete` fires once a valid
/// non-force-reset session is in place; the caller uses that
/// signal to mount the app shell.
export function mountLoginOverlay(onComplete: () => void): void {
  const host = ensureHost();
  while (host.firstChild) host.removeChild(host.firstChild);
  host.style.display = "flex";

  const card = document.createElement("div");
  card.className = "auth-card";

  const brand = document.createElement("div");
  brand.className = "auth-brand";
  brand.textContent = "Nexus Edge AI";
  card.appendChild(brand);

  const subtitle = document.createElement("div");
  subtitle.className = "auth-subtitle";
  subtitle.textContent = "Sign in to continue";
  card.appendChild(subtitle);

  const form = document.createElement("form");
  form.className = "auth-form";
  form.autocomplete = "on";

  const userLabel = document.createElement("label");
  userLabel.className = "auth-label";
  userLabel.textContent = "Username";
  const userInput = document.createElement("input");
  userInput.className = "auth-input";
  userInput.type = "text";
  userInput.name = "username";
  userInput.autocomplete = "username";
  userInput.required = true;
  userInput.spellcheck = false;
  userInput.autocapitalize = "off";
  userLabel.appendChild(userInput);

  const passLabel = document.createElement("label");
  passLabel.className = "auth-label";
  passLabel.textContent = "Password";
  const passInput = document.createElement("input");
  passInput.className = "auth-input";
  passInput.type = "password";
  passInput.name = "password";
  passInput.autocomplete = "current-password";
  passInput.required = true;
  passLabel.appendChild(passInput);

  const submit = document.createElement("button");
  submit.className = "auth-submit";
  submit.type = "submit";
  submit.textContent = "Sign in";

  const error = document.createElement("div");
  error.className = "auth-error";
  error.setAttribute("role", "alert");
  error.style.display = "none";

  form.appendChild(userLabel);
  form.appendChild(passLabel);
  form.appendChild(error);
  form.appendChild(submit);
  card.appendChild(form);
  host.appendChild(card);

  // Focus the first empty field for keyboard-first ergonomics.
  setTimeout(() => userInput.focus(), 0);

  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    const username = userInput.value.trim();
    const password = passInput.value;
    if (!username || !password) return;

    submit.disabled = true;
    submit.textContent = "Signing in…";
    error.style.display = "none";

    authApi
      .login({ username, password })
      .then((tok) => {
        const session = sessionFromTokenResponse(tok);
        setSession(session);
        if (session.user.force_password_reset) {
          // Hand off to the force-reset modal. The overlay
          // stays mounted underneath so the user can't escape
          // into the shell until they pick a new password.
          mountForcePasswordResetModal(session, () => {
            host.style.display = "none";
            onComplete();
          });
        } else {
          host.style.display = "none";
          onComplete();
        }
      })
      .catch((e: unknown) => {
        const msg =
          e instanceof Error && /^401/.test(e.message)
            ? "Invalid username or password."
            : e instanceof Error && /^4\d\d/.test(e.message)
              ? "Sign in failed. Check your credentials and try again."
              : "Sign in failed. Try again in a moment.";
        error.textContent = msg;
        error.style.display = "block";
        passInput.value = "";
        passInput.focus();
      })
      .finally(() => {
        submit.disabled = false;
        submit.textContent = "Sign in";
      });
  });
}

/// Hide the login overlay (e.g. after the caller mounted the
/// shell themselves on a pre-existing session). Does NOT
/// remove the host element — the next `mountLoginOverlay` call
/// reuses it.
export function hideLoginOverlay(): void {
  const host = document.getElementById(HOST_ID);
  if (host) host.style.display = "none";
}

/// Re-show the overlay (e.g. after logout). Equivalent to
/// `mountLoginOverlay` but spelled to communicate intent at
/// the call site.
export function showLoginOverlay(onComplete: () => void): void {
  mountLoginOverlay(onComplete);
}

/// True iff a valid (non-force-reset) session is in place. The
/// boot sequence uses this to decide whether to skip straight
/// to the shell or render the overlay.
export function hasUsableSession(): boolean {
  const s = getSession();
  return s != null && !s.user.force_password_reset;
}

function ensureHost(): HTMLElement {
  let host = document.getElementById(HOST_ID);
  if (!host) {
    host = document.createElement("div");
    host.id = HOST_ID;
    host.className = "auth-overlay";
    document.body.appendChild(host);
  }
  return host;
}
