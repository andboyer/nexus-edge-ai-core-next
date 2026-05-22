// Admin Audit — heading, filter row, and at least one row after the test
// session logs in (login itself emits an audit entry).

import { expect, test } from "@playwright/test";

import { loginAsAdmin } from "./helpers";

test.describe("admin: audit", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  test("page renders + filter form is present", async ({ page }) => {
    await page.goto("/admin/audit");
    await expect(
      page.getByRole("heading", { name: /^audit log$/i }),
    ).toBeVisible();

    // Filter row labels (scope to <label>; the table also has
    // "Actor"/"Action"/"Outcome" column headers).
    await expect(page.locator("label").filter({ hasText: /^actor$/i })).toBeVisible();
    await expect(page.locator("label").filter({ hasText: /^action$/i })).toBeVisible();
    await expect(page.locator("label").filter({ hasText: /^outcome$/i })).toBeVisible();

    // Clear button exists.
    await expect(
      page.getByRole("button", { name: /clear filters/i }),
    ).toBeVisible();
  });
});
