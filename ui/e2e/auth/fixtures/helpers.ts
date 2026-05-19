// M6 Phase 2 Step 2.9d — shared helpers for the auth e2e specs.

import type { Page } from "@playwright/test";
import { expect } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIDECAR = join(__dirname, "..", ".engine-state.json");

interface EngineState {
  baseURL: string;
  statedir: string;
  pid: number | null;
  bootstrapUsername: string;
  bootstrapOtp: string | null;
}

/// Read the per-suite engine state sidecar that `global-setup`
/// wrote. Throws if it's missing or the OTP wasn't captured.
export function readEngineState(): EngineState & { bootstrapOtp: string } {
  if (!existsSync(SIDECAR)) {
    throw new Error(
      `engine-state sidecar missing at ${SIDECAR}; are you running outside the auth globalSetup?`,
    );
  }
  const state = JSON.parse(readFileSync(SIDECAR, "utf8")) as EngineState;
  if (!state.bootstrapOtp) {
    throw new Error("engine-state sidecar has no bootstrapOtp; setup must have failed");
  }
  return { ...state, bootstrapOtp: state.bootstrapOtp };
}

/// Fill in the login overlay form, submit, and wait for the
/// next surface to mount.
///
/// `nextSurface`:
///   * `"shell"`  — credential is GOOD and `force_password_reset = false`;
///     wait for the sidebar to appear.
///   * `"force-reset"` — credential is GOOD and the user is in a
///     forced-reset state (bootstrap admin on first login, or
///     any user who was just reset by an admin); wait for the
///     "Set a new password" modal.
///   * `"error"` — credential is BAD; wait for the inline
///     `.auth-error` banner to fill with text.
export async function loginViaOverlay(
  page: Page,
  opts: {
    username: string;
    password: string;
    nextSurface: "shell" | "force-reset" | "error";
  },
): Promise<void> {
  // The overlay is the only thing on screen pre-login. Use the
  // `Sign in` submit button as the readiness anchor — present
  // in every local/hybrid render path.
  const userField = page.locator("input[name=username]");
  const passField = page.locator("input[name=password]");
  const submit = page.getByRole("button", { name: /^sign in$/i });

  await expect(userField).toBeVisible({ timeout: 15_000 });
  await userField.fill(opts.username);
  await passField.fill(opts.password);
  await submit.click();

  if (opts.nextSurface === "shell") {
    // The shell mounts a `.sidebar` (M-Admin Phase 0 layout) as
    // its first DOM element. That's the cleanest "we're past
    // login" signal that doesn't depend on which tab the
    // user happens to land on.
    await expect(page.locator(".sidebar")).toBeVisible({ timeout: 15_000 });
  } else if (opts.nextSurface === "force-reset") {
    // The force-reset modal renders into `#force-password-reset-modal`
    // and contains the literal subtitle "Welcome <user> — please
    // pick a new password before continuing.". Anchor on the
    // visible card brand "Set a new password" which is unique.
    await expect(page.getByText("Set a new password")).toBeVisible({
      timeout: 15_000,
    });
  } else {
    // The error banner shares the `.auth-error` class with the
    // `?oidc_error=...` banner; scope to the one inside the
    // form so we don't false-match on a stale OIDC error.
    await expect(page.locator(".auth-form .auth-error")).toBeVisible({
      timeout: 10_000,
    });
  }
}

/// Drive the force-password-reset modal. Returns once the
/// modal is dismissed AND the shell sidebar is visible.
export async function completeForcePasswordReset(
  page: Page,
  opts: { oldPassword: string; newPassword: string },
): Promise<void> {
  // The modal has three `<input type="password">` fields in
  // order: current, new, confirm. We address them by
  // `:nth-of-type` since the modal markup doesn't give them
  // names (intentional — autofill on a force-reset would be
  // worse than helpful).
  const fields = page.locator("#force-password-reset-modal input[type=password]");
  await expect(fields).toHaveCount(3, { timeout: 5_000 });
  await fields.nth(0).fill(opts.oldPassword);
  await fields.nth(1).fill(opts.newPassword);
  await fields.nth(2).fill(opts.newPassword);

  await page
    .locator("#force-password-reset-modal")
    .getByRole("button", { name: /update password/i })
    .click();

  await expect(page.locator("#force-password-reset-modal")).toBeHidden({
    timeout: 15_000,
  });
  await expect(page.locator(".sidebar")).toBeVisible({ timeout: 15_000 });
}

/// Navigate to a tab via the hash route (`#cameras`, `#admin-users`,
/// etc.) and wait for the tab content to mount. The sidebar
/// link approach is fragile under "Show deleted" toggles and
/// admin-only filtering, so we hash-navigate directly.
export async function gotoTab(page: Page, tabId: string): Promise<void> {
  await page.goto(`/#${tabId}`);
}

/// Click "Sign out" in the topbar user-chip. Waits for the
/// login overlay to reappear.
export async function logoutViaTopbar(page: Page): Promise<void> {
  await page.getByRole("button", { name: /^sign out$/i }).click();
  await expect(page.locator("input[name=username]")).toBeVisible({
    timeout: 10_000,
  });
}

/// Stash an OTP from a `confirm()`+OTP-modal flow (e.g.
/// admin-side "Reset password"). Returns the OTP shown in the
/// read-only `.otp-field` input, then dismisses the modal.
export async function captureOtpFromModal(page: Page): Promise<string> {
  const otpField = page.locator(".otp-field");
  await expect(otpField).toBeVisible({ timeout: 10_000 });
  const value = await otpField.inputValue();
  expect(value, "OTP field was empty").toBeTruthy();
  // The OTP modal's "Done" button dismisses without confirming
  // anything destructive — safe to always click.
  await page.getByRole("button", { name: /^done$/i }).click();
  await expect(otpField).toBeHidden({ timeout: 5_000 });
  return value;
}
