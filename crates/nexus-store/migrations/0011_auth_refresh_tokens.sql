-- M6 identity (Phase 2 Step 2.4): refresh-token table.
--
-- The session model is two-tier:
--
--   1. **Access JWT (HS256, 15 min TTL)** — signed with the
--      existing `auth.admin_secret_path` HS256 secret. Carries
--      `sub` (user id), `role`, `iat`, `exp`, `jti`. Stateless
--      so every API request is one HMAC verify and a Postgres
--      read — no row lookup, no allocation.
--
--   2. **Opaque refresh token (32 random bytes, URL-safe
--      base64, 30 day rolling TTL)** — single-use, stored
--      hashed (SHA-256 hex) in THIS table. The login handler
--      returns it once; every subsequent rotate hashes the
--      submitted secret and looks it up here.
--
-- Rotation is single-use: the moment a refresh is exchanged
-- (POST /api/v1/auth/refresh), `rotated_at` flips to the
-- exchange timestamp and the new refresh token is inserted
-- with `parent_id = <old.id>` and the same `chain_id`. If a
-- caller ever submits a token whose `rotated_at IS NOT NULL`,
-- the entire chain is revoked: `UPDATE auth_refresh_tokens SET
-- revoked_at = NOW() WHERE chain_id = ?` — that's the replay
-- detector, and it's the reason `chain_id` exists.
--
-- Invariants:
--
--   * `token_hash` is the SHA-256 hex of the secret half. The
--     plaintext secret only ever lives in the client's cookie
--     jar and (briefly) the engine's response buffer. If the
--     DB leaks, the leak window is "an attacker has to brute
--     force a 256-bit secret" — i.e. zero.
--
--   * `parent_id` is `NULL` for the chain root (the token
--     issued at login) and `<previous.id>` for every rotation.
--     ON DELETE SET NULL because deleting an old row in the
--     chain shouldn't break the head's parent pointer.
--
--   * `user_id REFERENCES users(id) ON DELETE CASCADE` — when
--     an admin hard-deletes a user (Step 2.8) all their
--     refresh tokens vanish. Soft-delete sets users.deleted_at
--     and leaves tokens; the login handler must check
--     users.deleted_at before honouring a refresh.
--
--   * No CHECK on `expires_at > created_at` — sqlite CHECKs on
--     TEXT-encoded timestamps work but they're brittle across
--     timezones. The store layer is the only writer and uses
--     `now + cfg.access_ttl_secs` directly, so the constraint
--     is enforced by construction.
--
--   * `user_agent` / `ip` are best-effort capture for the
--     /admin/sessions admin UI ("see all live sessions, revoke
--     one"). NULL is fine; the engine doesn't gate on them.
--
-- Indices:
--
--   * `UNIQUE(token_hash)` — every lookup goes through hash.
--     Without UNIQUE a brute-force collision (impossible, but)
--     could create two rows that both match.
--   * `(user_id, revoked_at, expires_at)` — the
--     /admin/sessions list query: `WHERE user_id = ? AND
--     revoked_at IS NULL AND expires_at > NOW()`.
--   * `(chain_id)` — the replay-detector revoke query touches
--     every row in a chain.

CREATE TABLE auth_refresh_tokens (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,

    -- SHA-256 hex (64 chars) of the secret half of the
    -- refresh token. NEVER the secret itself.
    token_hash  TEXT NOT NULL,

    user_id     INTEGER NOT NULL
                REFERENCES users(id) ON DELETE CASCADE,

    -- Single random UUIDv7 per login. Every rotation copies
    -- the parent's chain_id, so all generations of one login
    -- session share one chain_id and can be revoked together
    -- on replay detection.
    chain_id    TEXT NOT NULL,

    -- The row this one rotated from. NULL on the chain root.
    parent_id   INTEGER
                REFERENCES auth_refresh_tokens(id) ON DELETE SET NULL,

    -- ISO-8601 UTC ms-precision. Caller computes `now + ttl`.
    expires_at  TEXT NOT NULL,

    -- ISO-8601 UTC ms-precision. NULL = token still live.
    rotated_at  TEXT,

    -- ISO-8601 UTC ms-precision. Set when the chain is killed
    -- (logout, replay detection, admin revoke). When non-NULL
    -- the token will be rejected even if rotated_at is NULL.
    revoked_at  TEXT,

    -- Best-effort metadata for the /admin/sessions UI.
    user_agent  TEXT,
    ip          TEXT,

    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE UNIQUE INDEX idx_auth_refresh_tokens_token_hash
    ON auth_refresh_tokens (token_hash);

-- /admin/sessions list query — "what live sessions does this
-- user have?" The order here matters: user_id is the equality
-- filter, revoked_at + expires_at are range filters.
CREATE INDEX idx_auth_refresh_tokens_user_active
    ON auth_refresh_tokens (user_id, revoked_at, expires_at);

-- Replay-detector revoke hot path.
CREATE INDEX idx_auth_refresh_tokens_chain
    ON auth_refresh_tokens (chain_id);
