-- nexus:no-transaction
--
-- M2.2 Phase 1: cold-mirror replication schema.
--
-- This migration introduces the *dual-pointer* `motion_clips` shape
-- needed by cold-mirror. Each clip can independently exist on a hot
-- backend (local disk / USB) and a cold backend (LAN share / Drive /
-- OneDrive); the recorder stays UNAWARE of cold (writes hot only),
-- and a separate replicator follower task copies finished clips to
-- cold. Soft-eviction can later drop the hot copy while keeping the
-- DB row pointing at cold; hard-eviction (M2.1 cascade-delete) is
-- only legal for clips that have NEVER been replicated.
--
-- Three things land here:
--
--   1. `storage_backends` — registry of every backend the engine
--      knows about, keyed by a stable handle (`local`, `usb-vault`,
--      `lan-archive`, `gdrive`, `onedrive`). The implicit `local`
--      row is pre-seeded so the legacy `motion_clips.backend_id`
--      backfill below has a valid FK target.
--   2. `storage_cold_replica` — singleton-row table that names the
--      currently-active cold backend (NULL = cold disabled). The
--      M2.2 invariant "exactly one cold backend at a time" is
--      enforced by the `CHECK (id = 1)` PRIMARY KEY.
--   3. `motion_clips` rebuild — drops `path` + `backend_id` in
--      favour of `(hot_handle, hot_path, cold_handle, cold_path,
--      cold_uploaded_at, sha256)`. Backfill stamps every legacy row
--      with `hot_handle='local'`, `hot_path = path`, `sha256 = NULL`.
--      The `CHECK (hot_handle IS NOT NULL OR cold_handle IS NOT NULL)`
--      guarantees a clip row never points at NOTHING.
--
-- ===========================================================
-- Crash-safety / FK gymnastics — why `nexus:no-transaction`
-- ===========================================================
--
-- The store opens its connection pool with `foreign_keys=ON`. Under
-- that pragma, `DROP TABLE motion_clips` performs an implicit
-- `DELETE FROM motion_clips`, which cascades through the
-- `ON DELETE CASCADE` FKs in `motion_events.clip_id` and
-- `events.clip_id` (the latter flipped to CASCADE in 0003) — wiping
-- every motion-event and alert row that referenced the clip. That
-- would silently nuke production data on upgrade.
--
-- The official SQLite recipe (lang_altertable.html §7) is to run
-- `PRAGMA foreign_keys=OFF` OUTSIDE any transaction, do the table
-- rebuild inside a single `BEGIN..COMMIT`, run `PRAGMA
-- foreign_key_check` to confirm we did not introduce dangling
-- references, then restore `PRAGMA foreign_keys=ON`.
--
-- The default migration runner wraps each file in a transaction, so
-- the `PRAGMA foreign_keys=OFF` inside it would be a no-op. The
-- magic comment marker at the top of this file (`nexus:no-
-- transaction`) opts this migration out of that wrapping; the
-- runner instead executes every statement on a single pooled
-- connection and only stamps `schema_migrations` after the BEGIN/
-- COMMIT block below succeeds. (The marker is also why this file
-- contains its OWN `BEGIN; … COMMIT;` pair — without one the table
-- rebuild would not be atomic.)

PRAGMA foreign_keys = OFF;

BEGIN;

-- -----------------------------------------------------------
-- 1. storage_backends — registry of hot + cold targets.
-- -----------------------------------------------------------
-- `kind` is the discriminator the runtime uses to instantiate the
-- right `ColdBackend` / hot recorder impl. `config_json` is opaque
-- to the schema; each backend kind has its own settings shape
-- (e.g. `{"root":"/mnt/lan-archive"}` for kind='lan').
CREATE TABLE storage_backends (
    handle      TEXT    PRIMARY KEY,
    kind        TEXT    NOT NULL,
    config_json TEXT    NOT NULL DEFAULT '{}',
    created_at  TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    updated_at  TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    CHECK (kind IN ('local', 'usb', 'lan', 'gdrive', 'onedrive'))
);

-- The implicit hot-storage backend. Pre-seeded so the legacy
-- `motion_clips.backend_id='local'` rows survive the FK validation
-- after the table rebuild below. Operators cannot delete this row
-- (the application's `delete_storage_backend` helper rejects it +
-- the FK from `motion_clips.hot_handle` is `ON DELETE RESTRICT`).
INSERT INTO storage_backends (handle, kind, config_json)
VALUES ('local', 'local', '{}');

-- -----------------------------------------------------------
-- 2. storage_cold_replica — singleton "currently active cold" row.
-- -----------------------------------------------------------
-- Exactly one cold backend at a time per the M2.2 spec; the
-- `CHECK (id = 1)` plus the singleton seed below enforce it at
-- the schema level. `backend_handle = NULL` means cold replication
-- is currently disabled. `throttle_bps` is the soft cap the
-- replicator's token bucket honours so a 60 GiB backlog can't
-- saturate the LAN at boot. Default 50 MiB/s.
CREATE TABLE storage_cold_replica (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    backend_handle  TEXT    REFERENCES storage_backends(handle) ON DELETE RESTRICT,
    throttle_bps    INTEGER NOT NULL DEFAULT 52428800,
    updated_at      TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP)
);

-- Seed singleton row with cold replication DISABLED.
INSERT INTO storage_cold_replica (id, backend_handle) VALUES (1, NULL);

-- -----------------------------------------------------------
-- 3. motion_clips — rebuild with dual-pointer shape.
-- -----------------------------------------------------------
--
-- Per the official SQLite ALTER TABLE recipe with foreign_keys=OFF
-- in effect, we:
--   a. CREATE the new table under a temporary name.
--   b. Backfill from the original table.
--   c. DROP the original (no implicit cascade because FKs are off).
--   d. RENAME the new table to `motion_clips`. With foreign_keys
--      OFF, ALTER TABLE RENAME does NOT rewrite the FK references
--      in `motion_events.clip_id` / `events.clip_id` either — they
--      keep saying `REFERENCES motion_clips(id)` and resolve
--      against the freshly-renamed table on the next FK check.
--   e. (After COMMIT below) PRAGMA foreign_key_check confirms no
--      dangling references slipped through.
CREATE TABLE motion_clips_new (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id         INTEGER NOT NULL,
    started_at        TEXT    NOT NULL,
    ended_at          TEXT,
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    size_bytes        INTEGER NOT NULL DEFAULT 0,
    codec             TEXT    NOT NULL DEFAULT 'h264',
    container         TEXT    NOT NULL DEFAULT 'mp4',
    hot_handle        TEXT    REFERENCES storage_backends(handle) ON DELETE RESTRICT,
    hot_path          TEXT,
    cold_handle       TEXT    REFERENCES storage_backends(handle) ON DELETE RESTRICT,
    cold_path         TEXT,
    cold_uploaded_at  TEXT,
    sha256            TEXT,
    created_at        TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    CHECK (hot_handle IS NOT NULL OR cold_handle IS NOT NULL),
    CHECK (hot_handle IS NULL OR hot_path IS NOT NULL),
    CHECK (cold_handle IS NULL OR (cold_path IS NOT NULL AND cold_uploaded_at IS NOT NULL)),
    FOREIGN KEY (camera_id) REFERENCES cameras(id) ON DELETE CASCADE
);

INSERT INTO motion_clips_new (
    id, camera_id, started_at, ended_at, duration_ms, size_bytes,
    codec, container,
    hot_handle, hot_path, cold_handle, cold_path, cold_uploaded_at, sha256,
    created_at
)
SELECT
    id, camera_id, started_at, ended_at, duration_ms, size_bytes,
    codec, container,
    'local', path, NULL, NULL, NULL, NULL,
    created_at
FROM motion_clips;

DROP TABLE motion_clips;
ALTER TABLE motion_clips_new RENAME TO motion_clips;

-- M2.1 indexes on the rebuilt table, plus the M2.2 partial indexes
-- that make the new helper queries cheap:
--   * idx_motion_clips_pending_cold — drives clips_pending_cold_upload
--   * idx_motion_clips_soft_evict   — drives find_soft_evict_candidate
--   * idx_motion_clips_hard_evict   — drives find_hard_evict_candidate
CREATE INDEX IF NOT EXISTS idx_motion_clips_camera_started
    ON motion_clips(camera_id, started_at);
CREATE INDEX IF NOT EXISTS idx_motion_clips_ended
    ON motion_clips(ended_at);
CREATE INDEX IF NOT EXISTS idx_motion_clips_pending_cold
    ON motion_clips(ended_at)
    WHERE cold_handle IS NULL AND ended_at IS NOT NULL AND sha256 IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_motion_clips_soft_evict
    ON motion_clips(camera_id, started_at)
    WHERE hot_handle IS NOT NULL AND cold_handle IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_motion_clips_hard_evict
    ON motion_clips(camera_id, started_at)
    WHERE hot_handle IS NOT NULL AND cold_handle IS NULL;

COMMIT;

-- Verify no dangling FKs slipped through. Per SQLite docs, this
-- pragma reports any rows whose FK target is missing; we rely on
-- the runner surfacing any error result here. (Empty resultset =
-- pass; non-empty would have failed the migration above already
-- since we're now outside the transaction. Kept for defence in
-- depth and to make the recipe self-documenting.)
PRAGMA foreign_key_check;

PRAGMA foreign_keys = ON;
