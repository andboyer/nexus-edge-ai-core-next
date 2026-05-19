// M7 Step 6F — rules list-page Delivery chip e2e.
//
// Verifies Step 6E: every rule that has no per-rule override
// renders an `inherit` chip in the Delivery column. We use the
// seed rule (`any_person`) that the global-setup config injects;
// the engine creates an `inherit` policy by default for any rule
// the operator hasn't explicitly overridden.

import { test, expect } from "@playwright/test";

test.describe("M7 / rules list: Delivery chip", () => {
  test("seed rule renders an inherit chip", async ({ page }) => {
    await page.goto("/#rules");

    // Wait for the rules table; it's gated by api.rules.list() +
    // N parallel api.delivery.getRule(id) calls (Step 6E), so
    // give it a moment.
    const table = page.locator("table.admin-table").first();
    await expect(table).toBeVisible({ timeout: 15_000 });

    // The seed rule lives in the first body row. Both "inherit"
    // and "override" chips share the `.state-pill` slot; we
    // assert the specific delivery class so we don't accidentally
    // count the "enabled" pill in the prior column.
    const seedRow = table.locator("tbody tr", { hasText: "any_person" }).first();
    await expect(seedRow).toBeVisible({ timeout: 10_000 });

    const inheritChip = seedRow.locator(".delivery-chip-inherit");
    await expect(inheritChip).toHaveText(/inherit/i);
    await expect(inheritChip).toBeVisible();

    // Sanity-check the column header survived our 6E edit.
    await expect(table.locator("thead th", { hasText: /^delivery$/i })).toBeVisible();
  });
});
