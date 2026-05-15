-- M2.2 Phase 3 closeout: persistent runtime settings table.
--
-- Some operator-tunable knobs need to outlive a single `nexus-engine`
-- process without forcing a `nexus.toml` edit + restart. The
-- preferred USB hot-tier label is the first such knob (set via the
-- Storage Admin UI when the operator plugs in a `NEXUS_*`-labeled
-- stick mid-deployment). A generic key-value shape keeps the schema
-- stable as future settings land — adding a new key is an INSERT, no
-- migration required.
--
-- ## Lookup semantics
--
-- The engine resolves a setting at boot in this priority order:
--
--   1. `engine_runtime_settings(key=…)` row, if present.
--   2. The matching field in `nexus.toml` (e.g.
--      `runtime.clips.preferred_usb_label`).
--   3. Hard-coded default (typically `NULL` / disabled).
--
-- An operator-facing PUT lands the row in (1), which wins on the
-- next read; deleting the row falls back to (2). The engine never
-- writes back to the TOML.
--
-- ## Why NOT singleton-per-setting tables
--
-- The M2.2 spec uses singleton-row tables for `storage_cold_replica`
-- because that table has a multi-field row + strict invariants
-- (exactly one cold backend at a time). Runtime settings are
-- semantically scalar — `(key, value)` rows match the schema-as-data
-- shape better and don't multiply migration files.

CREATE TABLE engine_runtime_settings (
    key        TEXT PRIMARY KEY,
    -- NULL means "explicitly cleared" (operator hit the Clear
    -- button). Distinct from a row that doesn't exist, which falls
    -- back to TOML. The PUT handler stores NULL for clears.
    value      TEXT,
    updated_at TEXT NOT NULL DEFAULT (CURRENT_TIMESTAMP)
);
