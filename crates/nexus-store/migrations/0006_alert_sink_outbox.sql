-- M7 alert delivery: persistent outbox.
--
-- Every alert × every configured sink lands one row in this table,
-- inserted in the SAME `sqlx` transaction as `events`. The
-- dispatcher (a separate background task) drains pending rows,
-- calls `AlertSink::deliver`, and updates the row to one of the
-- terminal statuses below.
--
-- Hard invariants (enforced at the schema level):
--
--   1. **One row per (event, sink)** — `UNIQUE (event_id, sink_id)`.
--      A repeat enqueue of the same pair (e.g. a retry that races
--      a crash-recovery sweep) is rejected, not duplicated.
--
--   2. **Event-cascade.** When `events.event_id` is deleted (which
--      happens transitively when a `motion_clips` row is evicted
--      via the M2.1 watermark sweeper — see migrations 0002 and
--      0003), the outbox row goes with it. Without this the
--      dispatcher could be left holding a row pointing at a
--      missing event.
--
--   3. **`status` is a closed enum.** SQLite CHECK on the column
--      catches any typo at INSERT/UPDATE time so a buggy
--      dispatcher cannot land an unknown state in production.
--
--   4. **`suppression_reason` exists only when `status='suppressed'`.**
--      The CHECK below pairs the two so a `sent` row cannot carry a
--      stale reason from a prior cycle.
--
-- The partial index covers the dispatcher's hot path
-- (`status='pending' AND next_attempt_at <= now()`); a full-table
-- index would waste space on the `sent`/`dead` terminal rows that
-- accumulate.

CREATE TABLE alert_sink_outbox (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id           TEXT    NOT NULL REFERENCES events(event_id) ON DELETE CASCADE,
    sink_id            TEXT    NOT NULL,
    status             TEXT    NOT NULL DEFAULT 'pending',
    attempts           INTEGER NOT NULL DEFAULT 0,
    next_attempt_at    TEXT,
    last_error         TEXT,
    suppression_reason TEXT,
    created_at         TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    delivered_at       TEXT,
    UNIQUE (event_id, sink_id),
    CHECK (status IN ('pending', 'sent', 'failed', 'dead', 'suppressed')),
    CHECK (
        (status = 'suppressed' AND suppression_reason IS NOT NULL)
        OR (status <> 'suppressed' AND suppression_reason IS NULL)
    ),
    CHECK (
        suppression_reason IS NULL
        OR suppression_reason IN (
            'global_disabled',
            'rule_disabled',
            'off_schedule_global',
            'off_schedule_rule'
        )
    )
);

-- Hot-path index for the dispatcher's drain query:
--   SELECT * FROM alert_sink_outbox
--    WHERE status='pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?)
--    ORDER BY id LIMIT ?
-- Partial so terminal rows ('sent'/'dead'/'suppressed') don't
-- bloat the index as the outbox accumulates history.
CREATE INDEX idx_alert_sink_outbox_pending
    ON alert_sink_outbox (next_attempt_at, id)
    WHERE status = 'pending';

-- Event-side lookup for `GET /api/v1/events/:id/delivery` (per-event
-- delivery badges on the alert detail view).
CREATE INDEX idx_alert_sink_outbox_event
    ON alert_sink_outbox (event_id);
