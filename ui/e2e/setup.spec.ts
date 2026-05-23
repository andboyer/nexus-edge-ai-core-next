// First-boot setup wizard spec (M-Install Checkpoint 3c).
//
// globalSetup marks `engine_runtime_settings.setup_complete = "true"` so
// every OTHER spec lands on /dashboard without seeing the wizard. This
// spec flips the latch back off via the test-injection endpoint
// `POST /api/v1/_test/setup_reset`, walks the wizard end-to-end, and
// re-checks that the latch is flipped back on.
//
// IMPORTANT — this spec mutates global engine state in two ways that
// would break sibling specs if they ran AFTER it:
//   - It clears `setup_complete`. The wizard then sets it back via
//     `POST /v1/setup/complete` on Finish.
//   - It rotates the admin password from the bootstrap OTP to a new
//     stable value. The sidecar's `adminOtp` is no longer valid after
//     this spec runs; any sibling spec that uses `loginAsAdmin` will
//     fail.
//
// File ordering is the safety net: Playwright runs specs alphabetically
// with `workers: 1`, and `setup.spec.ts` sorts after every other file
// in `ui/e2e/`. If you ever add a spec whose name sorts AFTER `setup`,
// either rename it or move it to a separate Playwright project.

import { expect, test } from "@playwright/test";
import type { BrowserContext, Page } from "@playwright/test";

import { loginAsAdmin, readSidecar } from "./helpers";

// One-shot, serial. We reuse a single BrowserContext across all tests in
// this file because each step depends on the wizard state the previous
// step established. Playwright DOES NOT share contexts across `test()`
// calls by default, even in serial mode — hoist into beforeAll. See the
// user-memory note "Playwright trap" for the trap this avoids.
test.describe.configure({ mode: "serial" });

test.describe("first-boot setup wizard", () => {
  let context: BrowserContext;
  let page: Page;
  // The wizard requires us to type the OTP as the OLD password and pick
  // a new one. Keep both around so the post-flow assertions can sanity
  // check that the engine actually rotated the password.
  const newPassword = "wizard-acceptance-passphrase-2025";

  test.beforeAll(async ({ browser }) => {
    context = await browser.newContext();
    page = await context.newPage();
    await loginAsAdmin(page);

    // Reset the setup-complete latch via the test-injection endpoint so
    // the next /v1/setup/status call reports `setup_complete: false`.
    const { baseUrl } = readSidecar();
    const r = await fetch(`${baseUrl}/api/v1/_test/setup_reset`, {
      method: "POST",
    });
    expect(r.status).toBe(204);
  });

  test.afterAll(async () => {
    await context?.close();
  });

  test("router redirects to /setup when latch is unset", async () => {
    await page.goto("/dashboard");
    await expect(page).toHaveURL(/\/setup/, { timeout: 10_000 });
    // The Welcome step is identified by its unique CTA — the StepRail
    // breadcrumbs use the same step labels ("Welcome", "Password", …)
    // so plain text matching is ambiguous.
    await expect(
      page.getByRole("button", { name: /get started/i }),
    ).toBeVisible();
  });

  test("welcome step renders hostname + version, advances to password", async () => {
    // Already on /setup from the previous test. Hostname and version
    // are rendered into a definition list — assert that the labels are
    // present rather than pinning specific values (workdir-dependent).
    await expect(page.getByText(/hostname/i)).toBeVisible();
    await expect(page.getByText(/engine version/i)).toBeVisible();

    await page.getByRole("button", { name: /get started/i }).click();
    // Password step is identified by the "Change password" submit button.
    await expect(
      page.getByRole("button", { name: /change password/i }),
    ).toBeVisible();
  });

  test("password step is mandatory and rotates the bootstrap OTP", async () => {
    const { adminOtp } = readSidecar();

    // The Skip button must be hidden when force_password_reset is true.
    await expect(
      page.getByRole("button", { name: /^skip$/i }),
    ).toHaveCount(0);

    await page.getByLabel(/current password/i).fill(adminOtp);
    await page.getByLabel(/^new password$/i).fill(newPassword);
    await page.getByLabel(/confirm new password/i).fill(newPassword);
    await page.getByRole("button", { name: /change password/i }).click();

    // Engine responds 204; wizard advances to the cameras step. The
    // cameras step uniquely renders an "Add cameras" CTA button.
    await expect(
      page.getByRole("button", { name: /add cameras/i }),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("cameras step renders count and skips forward", async () => {
    // Fresh DB → no cameras configured. The CountStep renders the count
    // and the "Skip for now" / "Add cameras" buttons.
    await expect(page.getByText(/cameras configured/i)).toBeVisible();
    await page.getByRole("button", { name: /skip for now/i }).click();
    // Rules step is identified by its "Add rules" CTA.
    await expect(
      page.getByRole("button", { name: /add rules/i }),
    ).toBeVisible();
  });

  test("rules step renders count and skips forward", async () => {
    await expect(page.getByText(/rules configured/i)).toBeVisible();
    await page.getByRole("button", { name: /skip for now/i }).click();
    // Finish step is identified by its "Finish setup" button.
    await expect(
      page.getByRole("button", { name: /finish setup/i }),
    ).toBeVisible();
  });

  test("finish flips the latch and lands on /dashboard", async () => {
    const finishResponse = page.waitForResponse(
      (resp) =>
        resp.url().endsWith("/api/v1/setup/complete") &&
        resp.request().method() === "POST",
    );
    await page.getByRole("button", { name: /finish setup/i }).click();
    const resp = await finishResponse;
    expect(resp.status()).toBe(204);

    await expect(page).toHaveURL(/\/dashboard/, { timeout: 10_000 });
    await expect(page.locator("main")).toBeVisible();
  });

  test("re-visiting /setup after completion redirects back to /dashboard", async () => {
    await page.goto("/setup");
    await expect(page).toHaveURL(/\/dashboard/, { timeout: 10_000 });
  });

  test("password rotation persisted — bootstrap OTP no longer logs in", async () => {
    const { baseUrl, adminOtp } = readSidecar();
    const otpAttempt = await fetch(`${baseUrl}/api/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ username: "admin", password: adminOtp }),
    });
    expect(otpAttempt.status).toBeGreaterThanOrEqual(400);

    const newAttempt = await fetch(`${baseUrl}/api/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ username: "admin", password: newPassword }),
    });
    expect(newAttempt.ok).toBe(true);
  });
});
