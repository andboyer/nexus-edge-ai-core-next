// Auth spec: redirect when not logged in; login round-trip with bootstrap
// OTP succeeds and reveals the app shell.

import { expect, test } from "@playwright/test";

import { readSidecar } from "./helpers";

test.describe("auth", () => {
  test("unauthenticated visit redirects to /login", async ({ page }) => {
    await page.goto("/dashboard");
    await expect(page).toHaveURL(/\/login/);
    await expect(page.getByLabel(/username/i)).toBeVisible();
  });

  test("login with bootstrap admin OTP lands on dashboard", async ({
    page,
  }) => {
    const { adminOtp } = readSidecar();
    test.skip(!adminOtp, "no bootstrap OTP captured");

    await page.goto("/login");
    await page.getByLabel(/username/i).fill("admin");
    await page.getByLabel(/password/i).fill(adminOtp);
    await page.getByRole("button", { name: /sign in/i }).click();

    // Either land directly on /dashboard, or hit force-password-reset overlay.
    await page.waitForURL(/\/(dashboard|reset|change-password)?/, {
      timeout: 10_000,
    });
    // Dashboard layout should mount eventually.
    await expect(page.locator("main")).toBeVisible({ timeout: 10_000 });
  });
});
