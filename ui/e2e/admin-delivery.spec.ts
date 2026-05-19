// M7 Step 6F — admin-delivery paint-and-save e2e.
//
// Covers the most fragile UI surface in M7: the click-and-drag
// schedule painter + the two-way Save / load round-trip. We use
// the "Business hours" preset to paint the grid (deterministic
// and far cheaper than synthesising a 96-event mouse drag), then
// reload the page and assert the same set of cells comes back
// painted.
//
// Sibling specs cover the rest of the M7 surface end-to-end:
//   * `cascade-off-schedule.spec.ts` — empty global schedule
//     suppresses delivery with `off_schedule_global`.
//   * `cascade-rule-disabled.spec.ts` — per-rule disable
//     suppresses delivery with `rule_disabled`.
//   * `happy-path.spec.ts` — default cascade delivers to the
//     mock webhook + the per-event dialog renders `✓ sent`.
//   * `rules-delivery-chip.spec.ts` — rules list shows the new
//     `inherit` chip in the Delivery column.

import { test, expect } from "@playwright/test";

test.describe("M7 / admin-delivery: paint + save + reload", () => {
  test("Business hours preset round-trips through the engine", async ({ page }) => {
    await page.goto("/#admin-delivery");

    // The page rendered once the policy GET resolves. The Save
    // button is always present (even before the toggle flips the
    // grid on) so it's a stable readiness anchor.
    const saveBtn = page.getByRole("button", { name: /^save$/i });
    await expect(saveBtn).toBeVisible({ timeout: 15_000 });

    // A fresh DB returns `schedule: null` → the editor is hidden
    // until the "Restrict to a weekly schedule" toggle flips on.
    // Flip it, then wait for the editor to mount.
    const restrictToggle = page.getByLabel(/restrict.*weekly schedule/i);
    await expect(restrictToggle).toBeVisible();
    if (!(await restrictToggle.isChecked())) {
      await restrictToggle.check({ force: true });
    }
    const editor = page.locator(".schedule-editor").first();
    await expect(editor).toBeVisible({ timeout: 10_000 });

    // Click the "Business hours" preset and confirm it paints
    // exactly the expected number of cells.
    //   Mon-Fri (5 days) × slots 16..35 (20 half-hour slots) = 100.
    await page.getByRole("button", { name: /business hours/i }).click();
    await expect(page.locator(".schedule-cell.schedule-cell-on")).toHaveCount(100);

    // Snapshot the (d, s) pairs that are on before save so we can
    // compare exact-set equality after reload.
    const before = await collectPaintedCells(page);
    expect(before.length).toBe(100);

    // Save. The toast container is created on first toast push.
    await saveBtn.click();
    await expect(
      page.locator(".toast.toast-success", { hasText: /delivery settings saved/i }),
    ).toBeVisible({ timeout: 10_000 });

    // Full page reload — the engine is the source of truth now.
    // The schedule should re-hydrate automatically since the saved
    // payload includes `schedule: { grid: ... }`.
    await page.goto("/#admin-delivery");
    await expect(page.locator(".schedule-editor").first()).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".schedule-cell.schedule-cell-on")).toHaveCount(100, {
      timeout: 10_000,
    });
    const after = await collectPaintedCells(page);
    expect(new Set(after)).toEqual(new Set(before));
  });
});

// Return painted cells as "d:s" strings so the test can use exact
// set equality. The cells carry `data-d` (0..6) + `data-s` (0..47)
// per ui/src/lib/schedule-editor.ts.
async function collectPaintedCells(page: import("@playwright/test").Page): Promise<string[]> {
  return page.$$eval(".schedule-cell.schedule-cell-on", (els) =>
    els.map((el) => {
      const d = (el as HTMLElement).dataset["d"] ?? "?";
      const s = (el as HTMLElement).dataset["s"] ?? "?";
      return `${d}:${s}`;
    }),
  );
}
