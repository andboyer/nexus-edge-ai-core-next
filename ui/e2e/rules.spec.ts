// Rules page smoke + editor sheet round-trip.

import { expect, test } from "@playwright/test";

import { loginAsAdmin } from "./helpers";

test.describe("rules", () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  test("empty list + new-rule sheet opens with CEL textarea", async ({
    page,
  }) => {
    await page.goto("/rules");
    await expect(
      page.getByRole("heading", { name: /^rules$/i }),
    ).toBeVisible();

    await expect(page.getByText(/no rules configured/i)).toBeVisible();

    await page.getByRole("button", { name: /new rule/i }).click();
    await expect(
      page.getByRole("heading", { name: /^new rule$/i }),
    ).toBeVisible();

    // Identity placeholders confirm the sheet rendered.
    await expect(page.getByPlaceholder(/rule-person-zone/i)).toBeVisible();
    await expect(page.getByPlaceholder(/person in dwell zone/i)).toBeVisible();

    await page.getByRole("button", { name: /^cancel$/i }).click();
    await expect(
      page.getByRole("heading", { name: /^new rule$/i }),
    ).toBeHidden();
  });
});
