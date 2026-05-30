-- 0018_entity_local_state.sql
--
-- Phase 5.6 · R4 — persist `(camera_id, track_id) -> entity_local_id`
-- assignments so a crash + systemd restart within a short window
-- doesn't re-mint a fresh `entity_local_id` for what the tracker
-- still considers the same physical object. The cloud-side
-- identity-graph linker is authoritative for cross-camera + cross-
-- restart stitching via embedding similarity; this table is a
-- best-effort dedup hint inside one short window only.
--
-- HARD PII INVARIANT — DO NOT ADD COLUMNS WITHOUT WEDGE-PLAN REVIEW.
-- Per AGENTS.md rule 7 + the wedge plan's pseudonymous-by-default
-- substrate: this table MUST NOT carry `name`, `email`, `phone`,
-- `face_*`, or any other personal identifier alongside
-- `entity_local_id`. If the M6 admin surface ever lets an operator
-- attach a label to an entity, that label lives in a separate
-- `operator_entity_labels` table that NEVER replicates to the cloud.
-- See REPO_BOUNDARY.md R10 (cloud side) and the matching engine-side
-- code-review rule in AGENTS.md.
--
-- The table is single-row-per-`(camera_id, track_id)`. A track_id is
-- a per-camera ByteTrack id (NOT globally unique), so the composite
-- primary key is correct. ISO-formatted timestamps so the existing
-- chrono-bound queries lex-compare cleanly (cf. user-memory note on
-- SQLite `CURRENT_TIMESTAMP` vs RFC3339).
CREATE TABLE IF NOT EXISTS entity_local_state (
    camera_id        INTEGER NOT NULL,
    track_id         INTEGER NOT NULL,
    entity_local_id  TEXT    NOT NULL,
    started_ts       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_seen_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (camera_id, track_id)
);

CREATE INDEX IF NOT EXISTS idx_entity_local_state_last_seen
    ON entity_local_state (last_seen_at);
