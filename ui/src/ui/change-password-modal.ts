// M6 Phase 2 Step 2.9 — force-password-reset modal.
//
// Triggered when a fresh login response carries
// `user.force_password_reset = true` (set by the admin
// `POST /v1/admin/users/{id}/reset-password` flow, or on the
// initial admin-bootstrap row).
//
// Posture:
//
// * Blocks the entire UI — backdrop captures clicks, no
//   close button. The user CAN log out (top-right link) but
//   cannot reach the shell until they pick a new password.
// * `old_password` is required even right after login because
//   the engine's `change_password` handler verifies it before
//   touching the hash. We make the user re-type the OTP they
//   were just emailed; this also catches the rare case of a
//   shared workstation where the previous user left a session
//   open.
// * On success the engine returns a fresh (access, refresh)
//   pair AND clears `force_password_reset` server-side. We
//   swap the local session for the new one before dismissing.

import { auth as authApi } from "../api/auth.js";
import {
  type Session,
  logout,
  sessionFromTokenResponse,
  setSession,
} from "../lib/auth.js";

const HOST_ID = "force-password-reset-modal";

/// Mount the force-reset modal. `onComplete` fires once the
/// password change succeeds and the new session is committed
/// to local storage. The caller is responsible for then
/// mounting the app shell.
export function mountForcePasswordResetModal(
  session: Session,
  onComplete: () => void,
): void {
  const host = ensureHost();
  while (host.firstChild) host.removeChild(host.firstChild);
  host.style.display = "flex";

  const card = document.createElement("div");
  card.className = "auth-card";

  const title = document.createElement("div");
  title.className = "auth-brand";
  title.textContent = "Set a new password";
  card.appendChild(title);

  const subtitle = document.createElement("div");
  subtitle.className = "auth-subtitle";
  subtitle.textContent = `Welcome ${session.user.username} — please pick a new password before continuing.`;
  card.appendChild(subtitle);

  const form = document.createElement("form");
  form.className = "auth-form";
  form.autocomplete = "off";

  const oldLabel = document.createElement("label");
  oldLabel.className = "auth-label";
  oldLabel.textContent = "Current password (the one-time password you were given)";
  const oldInput = document.createElement("input");
  oldInput.className = "auth-input";
  oldInput.type = "password";
  oldInput.autocomplete = "current-password";
  oldInput.required = true;
  oldLabel.appendChild(oldInput);

  const newLabel = document.createElement("label");
  newLabel.className = "auth-label";
  newLabel.textContent = "New password (minimum 12 characters)";
  const newInput = document.createElement("input");
  newInput.className = "auth-input";
  newInput.type = "password";
  newInput.autocomplete = "new-password";
  newInput.required = true;
  newInput.minLength = 12;
  newLabel.appendChild(newInput);

  const confirmLabel = document.createElement("label");
  confirmLabel.className = "auth-label";
  confirmLabel.textContent = "Confirm new password";
  const confirmInput = document.createElement("input");
  confirmInput.className = "auth-input";
  confirmInput.type = "password";
  confirmInput.autocomplete = "new-password";
  confirmInput.required = true;
  confirmInput.minLength = 12;
  confirmLabel.appendChild(confirmInput);

  const submit = document.createElement("button");
  submit.className = "auth-submit";
  submit.type = "submit";
  submit.textContent = "Update password";

  const logoutBtn = document.createElement("button");
  logoutBtn.className = "auth-secondary";
  logoutBtn.type = "button";
  logoutBtn.textContent = "Sign out";

  const error = document.createElement("div");
  error.className = "auth-error";
  error.setAttribute("role", "alert");
  error.style.display = "none";

  form.appendChild(oldLabel);
  form.appendChild(newLabel);
  form.appendChild(confirmLabel);
  form.appendChild(error);
  form.appendChild(submit);
  form.appendChild(logoutBtn);
  card.appendChild(form);
  host.appendChild(card);

  setTimeout(() => oldInput.focus(), 0);

  logoutBtn.addEventListener("click", () => {
    void logout().then(() => {
      // Wipe the modal and let main.ts re-render the login
      // overlay via its onSessionChange subscription.
      host.style.display = "none";
    });
  });

  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    const oldPassword = oldInput.value;
    const newPassword = newInput.value;
    const confirm = confirmInput.value;

    if (newPassword !== confirm) {
      error.textContent = "New password and confirmation don't match.";
      error.style.display = "block";
      return;
    }
    if (newPassword.length < 12) {
      error.textContent = "New password must be at least 12 characters.";
      error.style.display = "block";
      return;
    }
    if (newPassword === oldPassword) {
      error.textContent = "New password must differ from the current one.";
      error.style.display = "block";
      return;
    }

    submit.disabled = true;
    logoutBtn.disabled = true;
    submit.textContent = "Updating…";
    error.style.display = "none";

    authApi
      .changePassword(
        { old_password: oldPassword, new_password: newPassword },
        session.access_token,
      )
      .then((tok) => {
        setSession(sessionFromTokenResponse(tok));
        host.style.display = "none";
        onComplete();
      })
      .catch((e: unknown) => {
        const raw = e instanceof Error ? e.message : String(e);
        let msg = "Couldn't update your password. Please try again.";
        if (/^401/.test(raw)) {
          msg = "Current password is incorrect.";
        } else if (/^400/.test(raw)) {
          // The engine's `PasswordPolicyError` is wrapped in
          // the `400 password_policy: <reason>` shape — pull
          // out the reason for the user.
          const m = /password_policy: ([^"\\]+)/.exec(raw);
          msg = m ? `Password rejected: ${m[1]}` : "Password rejected by policy.";
        }
        error.textContent = msg;
        error.style.display = "block";
      })
      .finally(() => {
        submit.disabled = false;
        logoutBtn.disabled = false;
        submit.textContent = "Update password";
      });
  });
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
