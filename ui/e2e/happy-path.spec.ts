// M7 Step 6F2 — happy-path delivery.
//
// The "everything works" canary. Default cascade (global enabled,
// no schedule, no per-rule override) → injected event should be
// dispatched to the configured webhook sink, the mock server
// should observe the HTTP POST, and the per-event Delivery dialog
// should render `✓ sent`.
//
// Catches the most dangerous silent-failure mode in M7: the
// cascade misconfigured to suppress everything. The 11 policy
// unit tests cover the math; this is the wiring sanity check.

import { expect, test } from "@playwright/test";
import {
  getMockCount,
  injectEvent,
  resetDeliveryDefaults,
  resetMockCounter,
  waitForDeliveryTerminal,
} from "./fixtures/helpers";

test.describe("M7 / happy path: webhook sent", () => {
  test.beforeEach(async ({ request }) => {
    await resetDeliveryDefaults(request, ["any_person"]);
    await resetMockCounter(request);
  });

  test("default cascade delivers to webhook + dialog shows sent", async ({
    page,
    request,
  }) => {
    // (1) No-op — default cascade is already permissive after
    // resetDeliveryDefaults in beforeEach. This block exists to
    // document the precondition.

    // (2) Fire one event.
    const { event_id, trace_id } = await injectEvent(request);

    // (3) Wait for the dispatcher to drain the row to terminal.
    const rows = await waitForDeliveryTerminal(request, event_id);
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows) {
      expect(row.status, `row=${JSON.stringify(row)}`).toBe("sent");
      expect(row.attempts).toBeGreaterThanOrEqual(1);
    }

    // (4) Mock webhook server saw the POST. This is the stronger
    // counterpart of the suppression assertions: we're proving
    // the dispatcher actually dialled, not just that the badge
    // says it did.
    const { count, last_event_id } = await getMockCount(request);
    expect(count, "mock webhook should have been called at least once").toBeGreaterThanOrEqual(1);
    expect(last_event_id).toBe(event_id);

    // (5) Per-event Delivery dialog renders the sent badge.
    await page.goto("/#events");
    const row = page.locator("tr", { hasText: trace_id }).first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await row.getByRole("button", { name: /^view$/i }).click();

    const dialog = page.locator(".delivery-dialog-body");
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    const badge = dialog.locator(".delivery-badge.delivery-badge-sent").first();
    await expect(badge).toBeVisible();
    await expect(badge).toContainText(/sent/i);
  });
});
