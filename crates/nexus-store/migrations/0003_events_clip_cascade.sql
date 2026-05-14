-- M2.1 closeout: flip events.clip_id from ON DELETE SET NULL to ON DELETE CASCADE.
--
-- Per docs/M2_STORAGE.md crash-safety rules: cascade-delete of a clip
-- must remove every linked row, including alert events that referenced
-- it. Carrying SET NULL left the half-deleted state the rest of the
-- design exists to prevent (alert row pointing at a missing clip).
--
-- SQLite cannot ALTER a column's foreign key behaviour in place; the
-- canonical fix is the table-rebuild dance below. We intentionally
-- avoid PRAGMA foreign_keys = OFF (sqlx connection-scoped, can't be
-- toggled inside a transaction) — the rebuild ordering keeps the FK
-- graph valid at every step:
--   1. CREATE events_new with the new FK semantics.
--   2. Copy rows from events into events_new (FK to motion_clips
--      satisfied because clip_ids are unchanged).
--   3. DROP events. Nothing references it (alert_sink_outbox is M7
--      future work, not yet created); the FK from motion_events to
--      motion_clips is unaffected.
--   4. RENAME events_new to events.
--   5. Recreate every index originally defined on the table.

CREATE TABLE events_new (
    event_id     TEXT    PRIMARY KEY,
    camera_id    INTEGER NOT NULL,
    rule_id      TEXT    NOT NULL,
    track_id     INTEGER,
    label        TEXT    NOT NULL,
    severity     TEXT    NOT NULL,
    frame_id     INTEGER NOT NULL,
    captured_at  TEXT    NOT NULL,
    trace_id     TEXT    NOT NULL,
    payload_json TEXT    NOT NULL,
    clip_id      INTEGER REFERENCES motion_clips(id) ON DELETE CASCADE,
    FOREIGN KEY (camera_id) REFERENCES cameras(id) ON DELETE CASCADE
);

INSERT INTO events_new (
    event_id, camera_id, rule_id, track_id, label, severity, frame_id,
    captured_at, trace_id, payload_json, clip_id
)
SELECT
    event_id, camera_id, rule_id, track_id, label, severity, frame_id,
    captured_at, trace_id, payload_json, clip_id
FROM events;

DROP TABLE events;

ALTER TABLE events_new RENAME TO events;

CREATE INDEX IF NOT EXISTS idx_events_camera_ts   ON events(camera_id, captured_at);
CREATE INDEX IF NOT EXISTS idx_events_rule_ts     ON events(rule_id, captured_at);
CREATE INDEX IF NOT EXISTS idx_events_severity_ts ON events(severity, captured_at);
CREATE INDEX IF NOT EXISTS idx_events_trace       ON events(trace_id);
CREATE INDEX IF NOT EXISTS idx_events_clip        ON events(clip_id);
