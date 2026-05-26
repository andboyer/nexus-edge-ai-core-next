-- 0013_cloud_enrollment.sql — local persistence of the cloud enrollment
-- artifact bundle. Populated by `nexus-engine enroll`; consumed by the
-- on-boot tunnel launcher in `nexus-engine` and the actor_token /
-- entitlement verifiers in `nexus-cloud-client`.
--
-- Single-row table — at most one cloud enrollment can be active at a
-- time. A re-enroll overwrites the existing row in place (no history
-- kept locally; the cloud-side `core_replacements` table is the source
-- of truth for replacement history).
--
-- All PEM blobs are stored verbatim; no parsing or canonicalisation on
-- write. The boot tunnel launcher hands them straight to
-- `nexus_cloud_client::tunnel::TunnelClient::new`.

CREATE TABLE IF NOT EXISTS cloud_enrollment (
    -- Constant; lets us use UPSERT with a known PK to enforce
    -- "at most one row".
    id                          INTEGER PRIMARY KEY CHECK (id = 1),
    -- Cloud-assigned core UUID. Returned by enrollment-svc.
    core_id                     TEXT    NOT NULL,
    -- WSS endpoint the engine dials on boot.
    gateway_url                 TEXT    NOT NULL,
    -- mTLS leaf certificate (signed by the internal CA).
    cert_pem                    TEXT    NOT NULL,
    -- PKCS#8 private key that pairs with `cert_pem`. The engine generated
    -- this locally during `enroll`; it never leaves the box.
    private_key_pem             TEXT    NOT NULL,
    -- Internal CA chain the engine validates the gateway cert against.
    ca_chain_pem                TEXT    NOT NULL,
    -- Initial entitlement JWT pre-loaded so the engine can run before
    -- the first heartbeat round-trip.
    entitlement_jwt             TEXT    NOT NULL,
    -- Optional Ed25519 verifying-key bundle for `actor_token` +
    -- `entitlement_update` JWTs. Both `signing_key_pem` and
    -- `signing_kid` are nullable so the row remains valid against the
    -- pre-1.8 enrollment response shape that omitted them.
    signing_key_pem             TEXT,
    signing_kid                 TEXT,
    -- Lex-sortable ISO 8601 to stay consistent with the rest of the
    -- store; sub-second precision matters for the `enrolled_at < ?`
    -- comparison in any future re-enroll cooldown check.
    enrolled_at                 TEXT    NOT NULL
        DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
