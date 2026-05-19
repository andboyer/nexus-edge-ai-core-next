-- M7 alert delivery (Step 5): global delivery settings.
--
-- Single-row table holding the global delivery controls:
--   - `enabled`     — master kill switch. When 0, every outbox row
--                     is suppressed with reason 'global_disabled'.
--                     Local recording is unaffected.
--   - `schedule_json` — optional weekly schedule (7 × 48 half-hour
--                     grid). NULL means "no schedule" (i.e. all
--                     times allowed, subject only to `enabled`).
--                     Shape: `{ "grid": bool[7][48] }`. Day index
--                     0 = Monday (chrono `Weekday::num_days_from_monday`).
--   - `timezone`    — IANA timezone name (e.g. `America/Los_Angeles`).
--                     Resolved via `chrono-tz` at policy-eval time
--                     so DST transitions are handled correctly.
--                     Defaults to 'UTC' if the engine can't detect
--                     the host's timezone at install.
--
-- The `CHECK (id = 1)` makes this a singleton table; UPSERTs target
-- the single row. The seed INSERT at the bottom ensures the row
-- always exists, so `delivery_settings_get` never has to handle a
-- "missing row" case.
--
-- Hot-reload: the engine watches the `delivery.settings.changed`
-- bus topic and rebuilds its in-memory `ArcSwap<DeliverySettings>`
-- on every signal. The admin handler `PUT /api/v1/admin/delivery`
-- is the only writer.

CREATE TABLE delivery_settings (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    enabled       INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    -- JSON-encoded `DeliverySchedule`. NULL = no schedule.
    schedule_json TEXT,
    timezone      TEXT    NOT NULL DEFAULT 'UTC',
    updated_at    TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP)
);

-- Seed the singleton row so `SELECT … WHERE id = 1` always hits.
INSERT INTO delivery_settings (id, enabled, schedule_json, timezone, updated_at)
VALUES (1, 1, NULL, 'UTC', CURRENT_TIMESTAMP);
