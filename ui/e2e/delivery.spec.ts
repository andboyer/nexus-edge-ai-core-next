// Delivery — heading and settings card both render (fresh DB has default
// settings: no schedule, no quiet hours).

import { expect, test } from "@playwright/test";

import { loginAsAdmin } from "./helpers";

test.describe("delivery", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  test("settings card renders", async ({ page }) => {
    await page.goto("/delivery");
    await expect(
      page.getByRole("heading", { name: /^delivery$/i }),
    ).toBeVisible();

    // Settings load is async → wait for one of the stable controls.
    await expect(
      page.getByLabel(/restrict.*weekly schedule/i),
    ).toBeVisible({ timeout: 10_000 });
  });
});
