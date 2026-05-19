// M6 Phase 2 Step 2.9d — local-mode auth + admin/users CRUD e2e.
//
// End-to-end round-trip covered by this spec (in declared order):
//
//   1. Bootstrap admin (`admin` + first-boot OTP scraped by
//      `global-setup`) logs in, completes the force-password-reset
//      modal, lands on the shell.
//   2. Admin creates an `operator` user (`alice`) with a
//      server-generated OTP. Alice logs out the admin, signs
//      in with her OTP, completes her own force-password-reset,
//      lands on the shell — but does NOT see the Users sidebar
//      entry (`requireAdmin: true`).
//   3. Admin signs back in, exercises the per-user CRUD flow:
//      role-change (operator → viewer → operator), reset
//      password (re-captures a new OTP via the modal), disable
//      then re-enable.
//   4. Alice's password has just been reset, so her chosen
//      password no longer works. Failing five login attempts
//      with the stale password trips the lockout FSM. Admin
//      logs in, sees the `locked` badge + Unlock button,
//      clicks Unlock, badge clears. Admin then deletes alice;
//      her row vanishes from the default list and reappears
//      under "Show deleted" with the tombstoned `(deleted)`
//      label.
//
// Single-worker / sequential — workers:1 in
// `playwright.auth.config.ts` + `fullyParallel: false`. The
// `LOCAL` object below is the inter-test state mailbox; this
// works because every test in the file runs in the same node
// process and Playwright honours declaration order.
//
// All credentials are session-local — they never leave the
// tempdir-rooted SQLite file the engine boots against. The
// bootstrap OTP is captured from the engine's stderr by
// `global-setup` and surfaced via the sidecar.

import { test, expect, type BrowserContext, type Page } from "@playwright/test";

import {
  captureOtpFromModal,
  completeForcePasswordReset,
  gotoTab,
  loginViaOverlay,
  logoutViaTopbar,
  readEngineState,
} from "./fixtures/helpers";

// Inter-test state. Module-level globals are OK here:
// `workers: 1` + sequential ordering guarantee each test
// runs after the prior one finishes.
interface SuiteState {
  /// The bootstrap OTP, after the admin force-reset replaces
  /// it with a real password.
  adminPassword: string;
  /// First OTP issued to alice when admin created her. Burned
  /// after her first login.
  aliceInitialOtp: string;
  /// Real password alice picks during her force-reset.
  alicePassword: string;
  /// Second OTP issued to alice when admin resets her password
  /// mid-suite (test 3). Replaces `aliceInitialOtp` as the
  /// "what alice would need to log in" credential — but in
  /// this spec we never actually use it (we lock her out by
  /// failing 5 attempts with the STALE password to exercise
  /// the lockout/unlock CRUD path).
  aliceResetOtp: string;
}

const LOCAL: SuiteState = {
  adminPassword: "",
  aliceInitialOtp: "",
  alicePassword: "",
  aliceResetOtp: "",
};

test.describe.configure({ mode: "serial" });

test.describe("M6 / local-mode auth + admin/users CRUD round-trip", () => {
  // Single persistent context + page across the whole suite —
  // Playwright's default is a fresh context per test, which
  // wipes localStorage and forces a re-login on every step.
  // We want the test 1 login to carry into tests 2-5 (until a
  // test explicitly signs out), so we own the context here.
  let context: BrowserContext;
  let page: Page;

  test.beforeAll(async ({ browser }) => {
    context = await browser.newContext();
    page = await context.newPage();
  });

  test.afterAll(async () => {
    await context.close();
  });

  test("bootstrap admin logs in, completes force-reset, lands on shell", async () => {
    const state = readEngineState();

    await page.goto("/");

    // First-boot row is created by `bootstrap_if_needed` with
    // `force_password_reset = true`, so a successful login
    // hands off straight to the force-reset modal.
    await loginViaOverlay(page, {
      username: state.bootstrapUsername,
      password: state.bootstrapOtp,
      nextSurface: "force-reset",
    });

    LOCAL.adminPassword = generatePassword("admin");
    await completeForcePasswordReset(page, {
      oldPassword: state.bootstrapOtp,
      newPassword: LOCAL.adminPassword,
    });

    // Admin sidebar surfaces the System → Users entry that
    // `requireAdmin: true` gates off for non-admins. Anchor
    // on the literal label so a future re-label of the route
    // is caught loudly.
    await expect(
      page.locator(".sidebar").getByRole("link", { name: /^users$/i }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("admin creates operator alice with server-generated OTP", async () => {
    await page.goto("/");

    // Session survives across tests because we never closed
    // the persistent `BrowserContext` — Playwright reuses one
    // per worker by default. The shell paint is the readiness
    // anchor; if a refresh-token race somehow logged us out,
    // the overlay would re-mount and the next line would fail
    // loudly on the "users link" assertion.
    await expect(page.locator(".sidebar")).toBeVisible({ timeout: 10_000 });

    await gotoTab(page, "admin-users");
    await expect(page.getByRole("heading", { name: /^users$/i })).toBeVisible({
      timeout: 10_000,
    });
    // The bootstrap row is always present on a fresh DB.
    await expect(
      page.locator(".users-table tbody tr").filter({ hasText: "admin" }),
    ).toBeVisible();

    // Open the "New user" modal. The username field is the
    // first input that lights up on mount (auto-focused).
    await page.getByRole("button", { name: /new user/i }).click();
    const usernameInput = page.locator(".dialog input[type=text]").first();
    await expect(usernameInput).toBeVisible({ timeout: 5_000 });
    await usernameInput.fill("alice");

    // Role select: operator. Default is "viewer" — flip it.
    await page.locator(".dialog select").selectOption("operator");

    // Leave the password blank → server generates an OTP and
    // returns it via the OTP-reveal modal.
    await page.locator(".dialog").getByRole("button", { name: /^create$/i }).click();

    LOCAL.aliceInitialOtp = await captureOtpFromModal(page);

    // Verify alice's row landed with `reset on next login`
    // status (force_password_reset = true).
    const aliceRow = page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" });
    await expect(aliceRow).toBeVisible({ timeout: 5_000 });
    await expect(aliceRow.locator(".badge", { hasText: /reset on next login/i }))
      .toBeVisible();
  });

  test("alice logs in with her OTP, force-resets, sees no Users sidebar entry", async () => {
    await page.goto("/");
    await logoutViaTopbar(page);

    await loginViaOverlay(page, {
      username: "alice",
      password: LOCAL.aliceInitialOtp,
      nextSurface: "force-reset",
    });

    LOCAL.alicePassword = generatePassword("alice");
    await completeForcePasswordReset(page, {
      oldPassword: LOCAL.aliceInitialOtp,
      newPassword: LOCAL.alicePassword,
    });

    // Non-admin → Users sidebar entry should be GONE. The
    // `requireAdmin: true` gate filters it out of `buildSidebar`
    // entirely.
    await expect(
      page.locator(".sidebar").getByRole("link", { name: /^users$/i }),
    ).toBeHidden();
    // Same for Audit Log — both are `requireAdmin: true`.
    await expect(
      page.locator(".sidebar").getByRole("link", { name: /^audit log$/i }),
    ).toBeHidden();

    // Direct hash-navigation as alice → the page mounts but
    // the list API returns 403 and the table swaps in the
    // "you don't have permission" banner.
    await gotoTab(page, "admin-users");
    await expect(
      page.getByText(/you don't have permission/i),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("admin signs back in and CRUDs alice (role / reset / disable / enable)", async () => {
    await page.goto("/");
    await logoutViaTopbar(page);

    await loginViaOverlay(page, {
      username: "admin",
      password: LOCAL.adminPassword,
      nextSurface: "shell",
    });

    await gotoTab(page, "admin-users");
    const aliceRow = page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" });
    await expect(aliceRow).toBeVisible({ timeout: 10_000 });

    // -------- Role-change: operator → viewer → operator --------
    const roleSelect = aliceRow.locator("select.role-select");
    await expect(roleSelect).toHaveValue("operator");
    await roleSelect.selectOption("viewer");
    // Toast surfaces on success; reload re-fetches the row.
    await expect(page.locator(".toast.toast-success")).toBeVisible({
      timeout: 5_000,
    });
    // After reload, the select that's bound to the same row
    // settles on the new value.
    await expect(
      page
        .locator(".users-table tbody tr")
        .filter({ hasText: "alice" })
        .locator("select.role-select"),
    ).toHaveValue("viewer", { timeout: 10_000 });

    await page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" })
      .locator("select.role-select")
      .selectOption("operator");
    await expect(
      page
        .locator(".users-table tbody tr")
        .filter({ hasText: "alice" })
        .locator("select.role-select"),
    ).toHaveValue("operator", { timeout: 10_000 });

    // -------- Reset password (admin-issued OTP) --------
    // confirm() guards this action — accept on next prompt.
    page.once("dialog", (d) => void d.accept());
    await page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" })
      .getByRole("button", { name: /reset password/i })
      .click();
    LOCAL.aliceResetOtp = await captureOtpFromModal(page);
    expect(LOCAL.aliceResetOtp).not.toEqual(LOCAL.aliceInitialOtp);

    // The row should now show `reset on next login` again
    // (engine set `force_password_reset = true`).
    await expect(
      page
        .locator(".users-table tbody tr")
        .filter({ hasText: "alice" })
        .locator(".badge", { hasText: /reset on next login/i }),
    ).toBeVisible({ timeout: 10_000 });

    // -------- Disable → re-enable --------
    await page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" })
      .getByRole("button", { name: /^disable$/i })
      .click();
    await expect(
      page
        .locator(".users-table tbody tr")
        .filter({ hasText: "alice" })
        .locator(".badge", { hasText: /^disabled$/i }),
    ).toBeVisible({ timeout: 10_000 });

    // Re-enable so subsequent tests can still authenticate as
    // alice (locked-out → unlock flow in the next test
    // needs an account that can attempt login).
    await page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" })
      .getByRole("button", { name: /^enable$/i })
      .click();
    await expect(
      page
        .locator(".users-table tbody tr")
        .filter({ hasText: "alice" })
        .locator(".badge", { hasText: /^disabled$/i }),
    ).toBeHidden({ timeout: 10_000 });
  });

  test("alice lockout after 5 failed logins → admin unlocks → admin deletes", async () => {
    await page.goto("/");
    await logoutViaTopbar(page);

    // Five failed attempts using the (now-stale) `alicePassword`.
    // Alice's current credential is `aliceResetOtp`, so the
    // old password is rejected → after the 5th failure the
    // FSM trips and the engine returns the same generic
    // 401 + locks the row.
    for (let i = 0; i < 5; i++) {
      await loginViaOverlay(page, {
        username: "alice",
        password: LOCAL.alicePassword + "-wrong",
        nextSurface: "error",
      });
      // The error banner sticks around; clear it by re-entering
      // the form. The next `loginViaOverlay` call will re-fill
      // both fields, so no extra work needed here.
    }

    await loginViaOverlay(page, {
      username: "admin",
      password: LOCAL.adminPassword,
      nextSurface: "shell",
    });
    await gotoTab(page, "admin-users");

    // The `locked` badge appears alongside whatever other
    // status badges the row carries. Scope to the row that
    // contains both "alice" and the locked badge.
    const aliceRow = page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" });
    await expect(
      aliceRow.locator(".badge", { hasText: /^locked$/i }),
    ).toBeVisible({ timeout: 10_000 });

    // Unlock button only appears when `locked_until > now`.
    await aliceRow.getByRole("button", { name: /^unlock$/i }).click();
    await expect(
      page
        .locator(".users-table tbody tr")
        .filter({ hasText: "alice" })
        .locator(".badge", { hasText: /^locked$/i }),
    ).toBeHidden({ timeout: 10_000 });

    // -------- Delete alice (soft delete) --------
    page.once("dialog", (d) => void d.accept());
    await page
      .locator(".users-table tbody tr")
      .filter({ hasText: "alice" })
      .getByRole("button", { name: /^delete$/i })
      .click();

    // Default list (Show deleted off) → alice's row disappears.
    await expect(
      page
        .locator(".users-table tbody tr")
        .filter({ hasText: /^alice/i })
        .filter({ hasNotText: "(deleted)" }),
    ).toBeHidden({ timeout: 10_000 });

    // Flip "Show deleted" → tombstone row appears with
    // `(deleted)` label and `deleted` badge.
    await page.locator("#users-include-deleted").check();
    const deletedRow = page
      .locator(".users-table tbody tr")
      .filter({ hasText: "(deleted)" });
    await expect(deletedRow).toBeVisible({ timeout: 10_000 });
    await expect(
      deletedRow.locator(".badge", { hasText: /^deleted$/i }),
    ).toBeVisible();
  });
});

/// Make a deterministic-but-spec-scoped password that satisfies
/// the engine's policy (min 12 chars, not in the denylist).
/// `tag` lets us tell test-run passwords apart at a glance when
/// debugging — they're never shared with production data.
function generatePassword(tag: string): string {
  // 12+ chars, mixed case + digits, plus the spec-run nonce.
  // Nonce includes `Math.random()` so a re-run after a half-
  // baked teardown can't collide with a stale row.
  const nonce = Math.random().toString(36).slice(2, 8);
  return `Nx${tag.padEnd(5, "x")}${nonce}!9X`;
}
