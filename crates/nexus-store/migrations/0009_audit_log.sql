-- M6 identity + audit (Phase 1 Step 1.2): immutable audit log.
--
-- Every privileged mutation, login attempt, and security event in
-- the engine writes one row here. Rows are inserted in the same
-- `sqlx` transaction as the domain mutation so a failed handler
-- rolls back both halves atomically — see
-- `nexus_store::audit::record_audit_event`.
--
-- Hard invariants (enforced at the schema level):
--
--   1. **Denormalised actor.** `actor_kind`, `actor_id`, and
--      `actor_label` are columns on this table, NOT foreign keys
--      into `users` (which doesn't even exist until 0010 lands).
--      This is intentional: the M6 spec requires audit history to
--      survive user soft-delete + rename, and the cleanest way to
--      guarantee that is to never join through `users` for read
--      paths. The break-glass admin "show me what the now-disabled
--      OIDC admin did last Tuesday" use case only works if the
--      historical principal lives inside the audit row itself.
--
--   2. **`actor_kind` is a closed enum.** SQLite CHECK catches
--      typos at INSERT time. Same pattern the M7 outbox uses for
--      `status`.
--
--   3. **`outcome` is a closed enum.** Login success / failure /
--      denial all map onto one of three terminal values so reports
--      ("how many failed logins last week") can group cheaply.
--
--   4. **`action` is a free-form string.** Deliberately NOT a
--      CHECK list — new resources (visual prompts, sinks, ...)
--      get new action verbs without a schema migration. The
--      convention is `<resource>.<verb>` (`camera.update`,
--      `user.delete`, `login.success`, etc.) and the engine
--      enforces it in code.
--
--   5. **Before/after JSON blobs are optional.** Login events
--      have no payload diff; a `camera.update` carries both. The
--      handler is responsible for stripping secrets from the
--      stored JSON (password hashes, HMAC secrets, OAuth tokens)
--      before calling `record_audit_event`.
--
-- Retention: a daily sweeper deletes rows older than
-- `runtime.audit.retention_days` (default 365). The sweeper is a
-- follow-up step; the schema just needs to be sweep-friendly,
-- which the `created_at DESC` index covers.
--
-- Compat: migration 0001 created a placeholder `audit_log` with a
-- different shape (`actor TEXT`, `resource TEXT`, `diff_json TEXT`)
-- that no code ever wrote to. Drop it unconditionally — there
-- can't be any rows worth preserving since no insert path existed.
-- Its two indices (`idx_audit_resource_ts`, `idx_audit_actor_ts`)
-- vanish with it.

DROP TABLE IF EXISTS audit_log;

CREATE TABLE audit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,

    -- WHO
    actor_kind    TEXT    NOT NULL,
    -- For local users: stringified `users.id`. For OIDC users: the
    -- hashed OIDC subject (matches `users.oidc_subject`). For
    -- system actions (boot-time bootstrap, retention sweeper):
    -- NULL. For legacy dev_token: a stable string like
    -- `"dev-token:cli"`.
    actor_id      TEXT,
    -- Human-readable label as it was at the time of the action.
    -- Captures username for local users, `preferred_username`
    -- claim for OIDC users, "system" for engine-internal actions.
    -- Denormalised so user renames / deletes don't rewrite history.
    actor_label   TEXT    NOT NULL,

    -- WHAT
    action        TEXT    NOT NULL,
    resource_kind TEXT,
    resource_id   TEXT,
    before_json   TEXT,
    after_json    TEXT,
    outcome       TEXT    NOT NULL,

    -- WHERE / WHEN
    ip            TEXT,
    user_agent    TEXT,
    -- DEFAULT in ISO-8601 UTC with millisecond precision and a
    -- trailing `Z`, e.g. `2026-05-18T14:32:01.123Z`. Matches the
    -- shape `DateTime::<Utc>::to_rfc3339()` emits when Rust code
    -- binds explicit timestamps, so SQL `<` / `>=` filters
    -- compare *chronologically* (SQLite TEXT comparison is
    -- lexicographic — see `delete_audit_older_than` and the
    -- `since`/`until` filters in `list_audit_filtered`). SQLite's
    -- own `CURRENT_TIMESTAMP` would emit
    -- `YYYY-MM-DD HH:MM:SS` (note the space), which lex-sorts
    -- BEFORE any `T`-form bind and silently breaks every
    -- range query against this column.
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),

    CHECK (actor_kind IN ('local_user', 'oidc_user', 'dev_token', 'system')),
    CHECK (outcome IN ('success', 'failure', 'denied')),
    -- Paired-nullability: if one of resource_kind / resource_id
    -- is set, the other must be set too. Lets us cheaply assert
    -- "this row is about a resource" with a single column check.
    CHECK (
        (resource_kind IS NULL AND resource_id IS NULL)
        OR (resource_kind IS NOT NULL AND resource_id IS NOT NULL)
    )
);

-- Hot paths:
--   per-resource history panel (last 50 for a camera/rule/sink/user)
CREATE INDEX idx_audit_resource
    ON audit_log (resource_kind, resource_id, created_at DESC);
--   per-actor activity ("show me everything Alice did")
CREATE INDEX idx_audit_actor
    ON audit_log (actor_id, created_at DESC);
--   by action verb ("all login.success in the last hour")
CREATE INDEX idx_audit_action
    ON audit_log (action, created_at DESC);
--   global audit feed + retention sweeper
CREATE INDEX idx_audit_created_at
    ON audit_log (created_at DESC);
