-- 0016_cloud_enrollment_attach_replay.sql — Phase 2 · Step 2.9.
--
-- §21.2 Attach-existing-core flow. `nexus-engine enroll --keep-history`
-- writes an `attach_replay_after` timestamp into the single-row
-- `cloud_enrollment` table; the next `nexus-engine serve` boot sees
-- the column populated, waits for the cloud tunnel to come up, and
-- replays every locally-recorded `motion_clip` since that cutoff
-- through the cloud outbox as a `clip_replicated` envelope carrying
-- `payload.attached_history = true`. The cloud's
-- `alert-ingest::upsert_clip` (already idempotent via
-- `ON CONFLICT (core_id, edge_clip_id) DO UPDATE`) absorbs duplicate
-- replays cleanly. On replay success the engine NULLs the column;
-- on partial replay (crash mid-batch) the next boot replays the
-- whole window again — safe because of the cloud-side ON CONFLICT.
--
-- The column is nullable so existing rows from migration 0013 stay
-- valid; NULL means "no replay pending".

ALTER TABLE cloud_enrollment ADD COLUMN attach_replay_after TEXT;
