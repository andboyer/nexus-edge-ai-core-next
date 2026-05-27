-- nexus:no-transaction
--
-- Phase 2 Step 2.1b: extend `storage_backends.kind` CHECK constraint
-- to allow the new `azure_blob` discriminator.
--
-- Background:
--   The original 0004 migration pinned the kind enum to
--   ('local','usb','lan','gdrive','onedrive'). The Phase 2 wedge
--   ships a cloud-tunnel-managed `AzureBlobBackend` (see
--   `crates/nexus-storage-cloud/src/azure.rs`) registered under the
--   reserved handle `"cloud"` post-enrollment. We need a fresh
--   discriminator value so the backend round-trips through the
--   admin listing API + the cold-replicator's `storage_cold_replica`
--   binding without tripping the CHECK.
--
-- SQLite cannot ALTER a CHECK constraint in place, so this follows
-- the same official ALTER TABLE recipe migration 0004 used:
--   * `foreign_keys=OFF` OUTSIDE any transaction (otherwise DROP
--     TABLE cascades to motion_clips.{hot,cold}_handle and
--     storage_cold_replica.backend_handle, nuking production data).
--   * Rebuild `storage_backends` inside a BEGIN..COMMIT block.
--   * `PRAGMA foreign_key_check` after the COMMIT to confirm no
--     dangling references slipped through (the runner surfaces a
--     non-empty result as an error).
--   * Restore `foreign_keys=ON`.
--
-- The magic marker on the first line opts this migration out of the
-- runner's default "wrap every file in BEGIN/COMMIT" behaviour, so
-- the PRAGMA actually applies. The runner stamps
-- `schema_migrations` after this file completes.
--
-- See `crates/nexus-store/migrations/0004_storage_backends.sql` for
-- the full rationale on why the no-transaction marker is necessary.

PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE storage_backends_new (
    handle      TEXT    PRIMARY KEY,
    kind        TEXT    NOT NULL,
    config_json TEXT    NOT NULL DEFAULT '{}',
    created_at  TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    updated_at  TEXT    NOT NULL DEFAULT (CURRENT_TIMESTAMP),
    CHECK (kind IN ('local', 'usb', 'lan', 'gdrive', 'onedrive', 'azure_blob'))
);

INSERT INTO storage_backends_new (handle, kind, config_json, created_at, updated_at)
SELECT handle, kind, config_json, created_at, updated_at
  FROM storage_backends;

DROP TABLE storage_backends;
ALTER TABLE storage_backends_new RENAME TO storage_backends;

COMMIT;

PRAGMA foreign_key_check;

PRAGMA foreign_keys = ON;
