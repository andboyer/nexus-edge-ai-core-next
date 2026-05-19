-- M6 identity (Phase 2 Step 2.1): local users table.
--
-- One row per principal that can hold a session in the engine.
-- Both local users (password_hash set, oidc_subject NULL) and
-- OIDC-only users (password_hash NULL, oidc_subject set) live
-- here. Hybrid mode (a local "break-glass" admin alongside OIDC
-- users) is the union of those two populations.
--
-- Hard invariants (enforced at the schema level):
--
--   1. **Every user has at least one auth method.**
--      `CHECK (password_hash IS NOT NULL OR oidc_subject IS NOT
--      NULL)` — a row with neither would be unauthenticatable
--      and is a bug, not a state we want to represent.
--
--   2. **Usernames are case-insensitive AND unique.** The
--      `UNIQUE` index uses SQLite's `COLLATE NOCASE` so
--      `"Alice"` and `"alice"` collide. M6 normalises to
--      lowercase before insert, but the index is the canonical
--      enforcer.
--
--   3. **`role` is a closed enum.** CHECK rejects typos; the
--      Rust `nexus_types::Role::FromStr` is the only path that
--      writes it.
--
--   4. **`oidc_subject` is hashed before storage.** OIDC `sub`
--      claims can leak identity if logged or breached; we store
--      SHA-256 (hex) instead. The UNIQUE index still works
--      because hash collisions on SHA-256 are not a thing we
--      need to plan for.
--
--   5. **Soft-delete via `deleted_at` + rename.** The spec
--      requires that a deleted user's audit history survive
--      ("breakglass admin can see what the now-disabled OIDC
--      admin did last Tuesday"). We renormalise the username
--      to `"<id>:deleted-<ts>"` at delete-time so the slot is
--      reusable. Audit log is already denormalised (M6 Phase 1
--      Step 1.2) so no FK rewrite is needed.
--
--   6. **No `last_admin_protection` in schema.** Last-admin
--      protection is enforced in `nexus-store::users` at the
--      function layer, not via a CHECK or trigger — the
--      "delete a user OR demote them to non-admin" check
--      requires counting peer admin rows which is awkward in
--      a CHECK. Test coverage in tests/users_schema.rs
--      exercises all three exit ramps (delete, demote,
--      disable).
--
-- Indices: lookup by username (case-insensitive, the login
-- hot path); lookup by oidc_subject (the OIDC callback hot
-- path); listing all active users by role for /admin/users
-- (operator can see viewers, etc — final shape TBD in 2.8).

CREATE TABLE users (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Canonical handle. NOCASE-collated UNIQUE index below.
    username               TEXT    NOT NULL,

    -- argon2id PHC string ("$argon2id$v=19$m=19456,t=2,p=1$..").
    -- NULL for OIDC-only users (no local password to verify).
    password_hash          TEXT,

    -- SHA-256 hex of the OIDC `sub` claim, scoped per-issuer if
    -- multiple IdPs are ever supported (right now there is one).
    -- NULL for local users (login flow never touches OIDC).
    oidc_subject           TEXT,

    role                   TEXT    NOT NULL DEFAULT 'viewer',

    -- True when admin reset the password OR on first-boot
    -- bootstrap. The /change-password endpoint clears it.
    force_password_reset   INTEGER NOT NULL DEFAULT 0,

    -- True when admin explicitly disabled the account.
    -- Distinct from `deleted_at IS NOT NULL` (disabled is
    -- reversible; deletion isn't).
    disabled               INTEGER NOT NULL DEFAULT 0,

    -- Lockout FSM state (Phase 2 Step 2.3). Both reset on
    -- successful login.
    failed_login_count     INTEGER NOT NULL DEFAULT 0,
    locked_until           TEXT,

    -- Telemetry — drives the /admin/users "last seen" column.
    last_login_at          TEXT,
    last_failed_login_at   TEXT,

    -- Audit shadow. created_at is what the UI shows; updated_at
    -- is touched on every mutation so the eventual "last
    -- modified" column doesn't need a trigger.
    created_at             TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at             TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    -- Soft-delete marker. NULL = active. When set, the
    -- corresponding username has been renamed to
    -- `<id>:deleted-<ts>` so the slot is reusable.
    deleted_at             TEXT,

    CHECK (role IN ('admin', 'operator', 'viewer')),
    CHECK (
        password_hash IS NOT NULL OR oidc_subject IS NOT NULL
    ),
    CHECK (disabled IN (0, 1)),
    CHECK (force_password_reset IN (0, 1)),
    CHECK (failed_login_count >= 0)
);

-- Canonical login lookup: by username, case-insensitive.
CREATE UNIQUE INDEX idx_users_username_nocase
    ON users (username COLLATE NOCASE);

-- OIDC callback lookup. Partial because local users have NULL
-- here and we don't want NULLs participating in the UNIQUE.
CREATE UNIQUE INDEX idx_users_oidc_subject
    ON users (oidc_subject)
    WHERE oidc_subject IS NOT NULL;

-- /admin/users sort + role-based listings.
CREATE INDEX idx_users_role_active
    ON users (role, deleted_at);
