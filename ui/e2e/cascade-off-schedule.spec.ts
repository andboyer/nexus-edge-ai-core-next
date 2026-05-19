// M7 Step 6F2 — cascade suppression via empty global schedule.
//
// Verifies the cascade math + the per-event Delivery dialog wiring
// end-to-end:
//
//   1. PUT global delivery settings with an all-false 7×48 grid
//      (effectively "deliveries are off, all the time").
//   2. Inject an AlertEvent that would otherwise fire deliveries.
//   3. Confirm the dispatcher marks the outbox row `suppressed`
//      with `suppression_reason = off_schedule_global`.
//   4. Confirm the mock webhook server received zero HTTP calls
//      (stronger assertion than "no UI badge appeared" — proves
//      we never dialled out, not just that the badge says we
//      didn't).
//   5. Confirm the per-event Delivery dialog renders the
//      `⊘ off-schedule (global)` badge.

import { expect, test } from "@playwright/test";
import {
  emptyScheduleGrid,
  getMockCount,
  injectEvent,
  resetDeliveryDefaults,
  resetMockCounter,
  waitForDeliveryTerminal,
} from "./fixtures/helpers";

test.describe("M7 / cascade: off_schedule_global", () => {
  test.beforeEach(async ({ request }) => {
    await resetDeliveryDefaults(request, ["any_person"]);
    await resetMockCounter(request);
  });

  test("empty schedule suppresses every delivery with off_schedule_global", async ({
    page,
    request,
  }) => {
    // (1) Lock the global cascade into "always off". The UI's
    // "Off" preset would produce the same payload; we PUT
    // directly because this spec doesn't test the painter (the
    // admin-delivery spec already does that).
    const putRes = await request.put("/api/v1/admin/delivery", {
      data: {
        enabled: true, // master switch on; the SCHEDULE blocks delivery
        schedule: { grid: emptyScheduleGrid() },
        timezone: "UTC",
      },
    });
    expect(putRes.ok(), `PUT /admin/delivery: ${putRes.status()}`).toBeTruthy();
    // Bus → reload task → ArcSwap takes a tick; wait a beat
    // before injecting so the dispatcher sees the new schedule.
    await page.waitForTimeout(300);

    // (2) Fire one event into the outbox.
    const { event_id, trace_id } = await injectEvent(request);

    // (3) Dispatcher should mark the row suppressed within a
    // tick or two. Returns once every row is terminal.
    const rows = await waitForDeliveryTerminal(request, event_id);
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows) {
      expect(row.status).toBe("suppressed");
      expect(row.suppression_reason).toBe("off_schedule_global");
    }

    // (4) Suppression happens BEFORE dial. The mock server must
    // not have seen a single request.
    const { count } = await getMockCount(request);
    expect(count, "mock webhook should not have been called").toBe(0);

    // (5) Now the UI assertion. Navigate to events, find the row
    // for our trace_id, click View, assert the badge.
    await page.goto("/#events");
    // Recent-events list reads 200 rows; our injected event has a
    // captured_at of "now" so it'll be at the top.
    const row = page.locator("tr", { hasText: trace_id }).first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await row.getByRole("button", { name: /^view$/i }).click();

    // Dialog opens with .delivery-dialog-body once the GET resolves.
    const dialog = page.locator(".delivery-dialog-body");
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    const badge = dialog.locator(".delivery-badge.delivery-badge-suppressed").first();
    await expect(badge).toBeVisible();
    await expect(badge).toContainText(/off-schedule \(global\)/i);
    // The tooltip carries the machine-readable reason so the
    // operator can grep audit logs by it.
    const title = await badge.getAttribute("title");
    expect(title ?? "").toContain("suppression=off_schedule_global");
  });
});
