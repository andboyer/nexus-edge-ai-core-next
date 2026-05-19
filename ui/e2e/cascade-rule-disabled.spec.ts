// M7 Step 6F2 — cascade suppression via per-rule disable.
//
// Verifies that a per-rule override with `enabled = false` wins
// over the global cascade and surfaces as `rule_disabled` on every
// delivery for that rule. Identical structure to
// `cascade-off-schedule.spec.ts`; the only behavioural difference
// is WHICH cascade gate fires.

import { expect, test } from "@playwright/test";
import {
  getMockCount,
  injectEvent,
  resetDeliveryDefaults,
  resetMockCounter,
  waitForDeliveryTerminal,
} from "./fixtures/helpers";

test.describe("M7 / cascade: rule_disabled", () => {
  test.beforeEach(async ({ request }) => {
    await resetDeliveryDefaults(request, ["any_person"]);
    await resetMockCounter(request);
  });

  test("per-rule disable suppresses every delivery with rule_disabled", async ({
    page,
    request,
  }) => {
    // (1) Set a per-rule override with delivery disabled. Schedule
    // omitted → undefined → inherit global (which is null + enabled).
    // The cascade ranks rule.enabled=false above any other gate, so
    // this is the only field we need to set.
    const putRes = await request.put("/api/v1/rules/any_person/delivery", {
      data: {
        policy: {
          enabled: false,
        },
      },
    });
    expect(putRes.ok(), `PUT /rules/any_person/delivery: ${putRes.status()}`).toBeTruthy();
    // Bus → reload task → ArcSwap takes a tick.
    await page.waitForTimeout(300);

    // (2) Fire one event.
    const { event_id, trace_id } = await injectEvent(request);

    // (3) Dispatcher should mark the row suppressed.
    const rows = await waitForDeliveryTerminal(request, event_id);
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows) {
      expect(row.status).toBe("suppressed");
      expect(row.suppression_reason).toBe("rule_disabled");
    }

    // (4) Mock server got nothing — disable wins before dial.
    const { count } = await getMockCount(request);
    expect(count, "mock webhook should not have been called").toBe(0);

    // (5) Per-event Delivery dialog renders the correct badge.
    await page.goto("/#events");
    const row = page.locator("tr", { hasText: trace_id }).first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await row.getByRole("button", { name: /^view$/i }).click();

    const dialog = page.locator(".delivery-dialog-body");
    await expect(dialog).toBeVisible({ timeout: 10_000 });
    const badge = dialog.locator(".delivery-badge.delivery-badge-suppressed").first();
    await expect(badge).toBeVisible();
    await expect(badge).toContainText(/rule delivery disabled/i);
    const title = await badge.getAttribute("title");
    expect(title ?? "").toContain("suppression=rule_disabled");
  });
});
