// M7 Step 6F2 — shared helpers for the cascade + happy-path specs.
//
// The three specs in this dir all need the same four building
// blocks: reset the cascade to a known state, fire an alert event
// via the dev-only `_test/inject_event` endpoint, wait for the
// dispatcher to drive the outbox row to terminal, and read the
// mock-webhook server's request counter. Hoisting them here keeps
// each spec focused on the ONE behavioural difference it's
// asserting.

import type { APIRequestContext } from "@playwright/test";
import { expect } from "@playwright/test";

/// Default mock-webhook URL the global setup wires up. We read
/// from the env var the setup script publishes; falling back to a
/// hardcoded value means a spec running in isolation outside the
/// fixture still fails LOUDLY with a clear "no E2E_MOCK_URL" error
/// rather than silently hitting a real host.
export function mockUrl(): string {
  const u = process.env["E2E_MOCK_URL"];
  if (!u) {
    throw new Error(
      "E2E_MOCK_URL not set — are you running outside the Playwright globalSetup?",
    );
  }
  return u;
}

/// Zero out the mock webhook's request counter so the spec can
/// assert "received exactly N calls" with no leakage from prior
/// specs (workers: 1, but specs share state).
export async function resetMockCounter(req: APIRequestContext): Promise<void> {
  const r = await req.post(`${mockUrl()}/_reset`);
  expect(r.ok(), `mock reset returned ${r.status()}`).toBeTruthy();
}

/// Read the mock webhook's current count + last_event_id.
export async function getMockCount(
  req: APIRequestContext,
): Promise<{ count: number; last_event_id: string | null }> {
  const r = await req.get(`${mockUrl()}/_count`);
  expect(r.ok(), `mock count returned ${r.status()}`).toBeTruthy();
  return r.json() as Promise<{ count: number; last_event_id: string | null }>;
}

/// Reset the cascade to a fully-permissive state: global enabled
/// + no schedule + UTC; rule override cleared. Use in beforeEach
/// so each spec starts from a known baseline regardless of what
/// the prior spec mutated.
export async function resetDeliveryDefaults(
  req: APIRequestContext,
  ruleIds: ReadonlyArray<string>,
): Promise<void> {
  // Global: enabled, no restriction.
  const rg = await req.put("/api/v1/admin/delivery", {
    data: { enabled: true, schedule: null, timezone: "UTC" },
  });
  expect(rg.ok(), `PUT /admin/delivery returned ${rg.status()}`).toBeTruthy();

  // Per-rule: clear any override so the rule inherits global.
  for (const id of ruleIds) {
    const rr = await req.put(`/api/v1/rules/${encodeURIComponent(id)}/delivery`, {
      data: { policy: null },
    });
    // 404 is acceptable here — the seed rule might already have no
    // override, and DELETE-by-PUT-null is idempotent on the server.
    expect(
      rr.ok() || rr.status() === 404,
      `PUT /rules/${id}/delivery returned ${rr.status()}`,
    ).toBeTruthy();
  }

  // The cascade reload task runs off a bus signal — give it one
  // tick to absorb the PUT before the test injects an event. The
  // dispatcher polls on a 1s tick, so 250ms is safe on a loaded
  // CI box without padding wall time.
  await sleep(250);
}

/// Build an empty 7×48 schedule (every slot off). The cascade
/// turns this into `off_schedule_global` on every delivery.
export function emptyScheduleGrid(): boolean[][] {
  return Array.from({ length: 7 }, () => Array<boolean>(48).fill(false));
}

/// Inject one alert event into the dispatcher via the dev-only
/// endpoint. Returns `{ event_id, trace_id }` — the trace_id is
/// what the events table renders so specs can find the right row.
export async function injectEvent(
  req: APIRequestContext,
  overrides: Partial<{
    rule_id: string;
    camera_id: number;
    label: string;
    severity: "low" | "med" | "high" | "critical";
  }> = {},
): Promise<{ event_id: string; trace_id: string }> {
  // `crypto.randomUUID()` returns a v4. The store accepts any
  // valid UUID — production uses v7 for time-ordering, but for a
  // single-injection spec ordering doesn't matter.
  const event_id = crypto.randomUUID();
  const trace_id = `e2e-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  const ev = {
    event_id,
    camera_id: overrides.camera_id ?? 1,
    rule_id: overrides.rule_id ?? "any_person",
    track_id: null,
    label: overrides.label ?? "person",
    severity: overrides.severity ?? "low",
    bbox: null,
    frame_id: 1,
    captured_at: new Date().toISOString(),
    trace_id,
    artifacts: {},
    context: {},
  };
  const r = await req.post("/api/v1/_test/inject_event", { data: ev });
  expect(r.ok(), `inject_event returned ${r.status()}: ${await r.text()}`).toBeTruthy();
  return { event_id, trace_id };
}

/// Poll `GET /api/v1/events/:id/delivery` until every row reaches
/// a terminal status (`sent`, `failed`, `dead`, `suppressed`) or
/// the timeout expires. Returns the rows for follow-up assertions.
export async function waitForDeliveryTerminal(
  req: APIRequestContext,
  eventId: string,
  timeoutMs = 15_000,
): Promise<
  Array<{
    id: number;
    sink_id: string;
    status: string;
    attempts: number;
    suppression_reason: string | null;
    last_error: string | null;
  }>
> {
  const deadline = Date.now() + timeoutMs;
  let last: unknown = null;
  while (Date.now() < deadline) {
    const r = await req.get(`/api/v1/events/${encodeURIComponent(eventId)}/delivery`);
    if (r.ok()) {
      const rows = (await r.json()) as Array<{
        id: number;
        sink_id: string;
        status: string;
        attempts: number;
        suppression_reason: string | null;
        last_error: string | null;
      }>;
      last = rows;
      // All rows terminal? Done.
      if (rows.length > 0 && rows.every((row) => row.status !== "pending")) {
        return rows;
      }
    }
    await sleep(150);
  }
  throw new Error(
    `waitForDeliveryTerminal: rows still pending after ${timeoutMs}ms; last=${JSON.stringify(last)}`,
  );
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
