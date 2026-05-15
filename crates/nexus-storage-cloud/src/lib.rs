//! Cloud cold-storage backends (M2.2 Phase 2).
//!
//! The leaf crate `nexus-storage` ships the [`ColdBackend`] trait
//! and a local-filesystem (`LanFsBackend`) impl. Cloud backends —
//! Google Drive, OneDrive — bring heavy HTTP/OAuth/TLS dependencies
//! (`reqwest`, `url`, `wiremock` for tests) that LAN-only
//! deployments have no business pulling. They live here so the
//! workspace stays cleanly tiered.
//!
//! ## What this crate does
//!
//! * Implements [`ColdBackend`] for Google Drive and Microsoft
//!   OneDrive against their respective REST APIs (Drive v3,
//!   Microsoft Graph v1.0).
//! * Provides [`build_cloud_backend`] — a factory the engine calls
//!   when [`nexus_storage::build_backend`] sees a cloud `kind`
//!   discriminator (`"gdrive"` / `"onedrive"`).
//! * Wraps the OAuth 2.0 **refresh-token grant** flow: the engine
//!   stores the long-lived refresh token (AES-GCM-encrypted at
//!   rest by [`nexus_storage::token_crypto`]), and the backend
//!   mints short-lived access tokens on demand with transparent
//!   refresh-on-401 retry for every API call.
//!
//! ## What this crate deliberately does NOT do
//!
//! * **No interactive UI for the OAuth dance.** The auth-code
//!   grant primitives ([`oauth::authorize_url`],
//!   [`oauth::exchange_code`], [`oauth::new_state`]) live here so
//!   `nexus-engine` can wire them into its admin HTTP API, but
//!   this crate doesn't bind any sockets, route any redirects, or
//!   bring up any UI. That's the engine + core-next UI's job.
//! * **No quota probing on the hot path.** The replicator already
//!   throttles via `TokenBucket`; we don't second-guess by
//!   `GET /about?fields=storageQuota` before every upload. Quota
//!   exhaustion surfaces as a `403` from the upload call, which
//!   maps to `BackendError::Other` and the replicator retries on
//!   the next tick.
//! * **No bidirectional sync.** Cold is write-only from the
//!   replicator's perspective. The optional `get_range` call is a
//!   read-back for the M2.2 Phase 4 cold-rehydrate cache; the
//!   engine never *modifies* a clip that's already on cold.
//!
//! ## Endpoint customisation
//!
//! All four base URLs (OAuth token endpoint, REST API base, upload
//! base) read from environment variables on construction so tests
//! can swap in a wiremock URL and operators can target Workspace
//! tenants, Azure Government, or the China clouds without code
//! changes:
//!
//! * `NEXUS_GDRIVE_TOKEN_URL`  (default `https://oauth2.googleapis.com/token`)
//! * `NEXUS_GDRIVE_API_BASE`   (default `https://www.googleapis.com/drive/v3`)
//! * `NEXUS_GDRIVE_UPLOAD_BASE` (default `https://www.googleapis.com/upload/drive/v3`)
//! * `NEXUS_ONEDRIVE_TOKEN_URL` (default `https://login.microsoftonline.com/common/oauth2/v2.0/token`)
//! * `NEXUS_ONEDRIVE_API_BASE`  (default `https://graph.microsoft.com/v1.0`)
//!
//! Env vars are read once per `build_cloud_backend` call.

#![forbid(unsafe_code)]

use std::sync::Arc;

use nexus_storage::token_crypto::{self, EncryptedToken, RefreshToken};
use nexus_storage::{BackendError, ColdBackend};
use serde::{Deserialize, Serialize};

pub mod gdrive;
pub mod oauth;
pub mod onedrive;

pub use gdrive::GoogleDriveBackend;
pub use oauth::{authorize_url, exchange_code, new_state, ExchangedTokens, Provider};
pub use onedrive::OneDriveBackend;

// ===========================================================================
// Wire-format types (what lives in `storage_backends.config_json`)
// ===========================================================================

/// Self-describing kind discriminator stored as
/// `storage_backends.kind`. The runtime DB CHECK constraint on
/// that column accepts these strings verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloudKind {
    /// Google Drive via Drive v3 REST API. Scope: `drive.file`.
    Gdrive,
    /// Microsoft OneDrive via Microsoft Graph v1.0. Scope:
    /// `Files.ReadWrite.AppFolder`.
    Onedrive,
}

impl CloudKind {
    /// Try to parse a `kind` string from `storage_backends.kind`.
    /// `None` means "not a cloud kind"; the engine falls through to
    /// [`nexus_storage::build_backend`] for filesystem kinds.
    ///
    /// Named `from_kind` rather than `from_str` so it doesn't shadow
    /// [`std::str::FromStr::from_str`] (which would invite calls
    /// through the standard trait that we don't actually implement).
    /// Mirrors [`oauth::Provider::from_kind`].
    pub fn from_kind(kind: &str) -> Option<Self> {
        match kind {
            "gdrive" => Some(Self::Gdrive),
            "onedrive" => Some(Self::Onedrive),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gdrive => "gdrive",
            Self::Onedrive => "onedrive",
        }
    }
}

/// Wire shape of `storage_backends.config_json` for a cloud
/// backend. The engine writes this, the cloud factory reads it.
///
/// `refresh_token` is the AES-GCM-encrypted blob produced by
/// [`nexus_storage::token_crypto::encrypt`]. Cleartext never lands
/// on disk; the factory decrypts it (using the admin-auth secret
/// HKDF-derived key) only after the row has been read into memory.
///
/// `client_id` / `client_secret` live next to the encrypted refresh
/// token because they're the credentials of the operator's
/// registered OAuth app (see `docs/M2_STORAGE.md` "OAuth client
/// registration"). They're treated as low-sensitivity — they get
/// you nowhere without a matching refresh token — but the engine
/// still gates the PUT endpoint behind admin auth so they can't be
/// trivially harvested.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudConfigJson {
    pub client_id: String,
    pub client_secret: String,
    /// AES-GCM-encrypted OAuth refresh token. Engine decrypts via
    /// [`nexus_storage::token_crypto::decrypt`] before constructing
    /// the backend; the factory in this crate accepts only the
    /// cleartext [`RefreshToken`] (see [`CloudConfig`]).
    pub refresh_token: EncryptedToken,
    /// Operator-visible label, surfaced in the admin UI's "connect
    /// status" string. Not used for any API call.
    #[serde(default)]
    pub account_email: Option<String>,
    /// Provider-specific extras. Always present; defaults to the
    /// empty variant for the matching kind.
    #[serde(default)]
    pub extra: serde_json::Value,
}

impl CloudConfigJson {
    /// Decrypt the refresh token using the admin-auth secret and
    /// produce a fully-constructed [`CloudConfig`] ready for
    /// [`build_cloud_backend`].
    pub fn decrypt(self, admin_secret: &str) -> Result<CloudConfig, BackendError> {
        let refresh = token_crypto::decrypt(admin_secret, &self.refresh_token).map_err(|e| {
            // The decrypt error variants deliberately collapse the
            // "wrong key" / "tampered ct" cases so we don't leak
            // structural info; surface as Auth so the UI prompts
            // for re-consent rather than a generic "backend down".
            BackendError::Auth(format!("decrypt refresh_token: {e}"))
        })?;
        Ok(CloudConfig {
            client_id: self.client_id,
            client_secret: self.client_secret,
            refresh_token: refresh,
            account_email: self.account_email,
            extra: self.extra,
        })
    }
}

/// Decrypted, ready-to-use cloud-backend config. Held in memory
/// only — never serialised back to disk in this form.
pub struct CloudConfig {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: RefreshToken,
    pub account_email: Option<String>,
    /// Untyped extra config, deserialised per-provider:
    ///
    /// * gdrive: `{ "root_folder_id": "<drive-folder-id>" | null }`
    ///   — when null/absent the backend uses the operator's
    ///   per-app implicit root folder (per-app sandbox via
    ///   `drive.file` scope, files appear under "My Drive" with the
    ///   app's name).
    /// * onedrive: `{ "tenant": "common" }` — the URL path segment
    ///   for the multi-tenant token endpoint (`common`, `consumers`,
    ///   `<tenant-id>`). Used only when `NEXUS_ONEDRIVE_TOKEN_URL`
    ///   is not set.
    pub extra: serde_json::Value,
}

impl std::fmt::Debug for CloudConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omit the refresh_token + client_secret so a
        // `tracing::info!(?cfg, ...)` can't leak the credentials.
        f.debug_struct("CloudConfig")
            .field("client_id", &self.client_id)
            .field("refresh_token", &self.refresh_token)
            .field("account_email", &self.account_email)
            .field("extra", &self.extra)
            .field("client_secret", &"<redacted>")
            .finish()
    }
}

// ===========================================================================
// Factory
// ===========================================================================

/// Construct an `Arc<dyn ColdBackend>` for a cloud `kind`. Called
/// by the engine's dispatch wrapper after it has read the row
/// from `storage_backends`, parsed [`CloudConfigJson`], and
/// decrypted the refresh token via the admin-auth secret.
///
/// `handle` matches `storage_backends.handle` and is the registry
/// key the cold replicator looks up against.
pub fn build_cloud_backend(
    handle: &str,
    kind: CloudKind,
    cfg: CloudConfig,
) -> Result<Arc<dyn ColdBackend>, BackendError> {
    match kind {
        CloudKind::Gdrive => Ok(Arc::new(GoogleDriveBackend::new(handle, cfg)?)),
        CloudKind::Onedrive => Ok(Arc::new(OneDriveBackend::new(handle, cfg)?)),
    }
}

/// Convenience: parse the config_json string + decrypt + build in
/// one call. The engine uses this from `rebuild_registry` so the
/// crypto + provider-dispatch logic stays out of `api.rs`.
pub fn build_from_config_json(
    handle: &str,
    kind: &str,
    config_json: &str,
    admin_secret: &str,
) -> Result<Arc<dyn ColdBackend>, BackendError> {
    let cloud_kind = CloudKind::from_kind(kind).ok_or_else(|| {
        BackendError::Other(format!(
            "build_from_config_json called with non-cloud kind '{kind}'",
        ))
    })?;
    let wire: CloudConfigJson = serde_json::from_str(config_json)
        .map_err(|e| BackendError::Other(format!("config_json parse: {e}")))?;
    let cfg = wire.decrypt(admin_secret)?;
    build_cloud_backend(handle, cloud_kind, cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_storage::token_crypto;

    #[test]
    fn cloud_kind_round_trip() {
        assert_eq!(CloudKind::from_kind("gdrive"), Some(CloudKind::Gdrive));
        assert_eq!(CloudKind::from_kind("onedrive"), Some(CloudKind::Onedrive));
        assert_eq!(CloudKind::from_kind("lan"), None);
        assert_eq!(CloudKind::from_kind(""), None);
        assert_eq!(CloudKind::Gdrive.as_str(), "gdrive");
        assert_eq!(CloudKind::Onedrive.as_str(), "onedrive");
    }

    #[test]
    fn config_json_round_trip_through_decrypt() {
        let secret = "test-admin-secret";
        let encrypted = token_crypto::encrypt(secret, "fake-refresh-token").unwrap();
        let wire = CloudConfigJson {
            client_id: "client-id".into(),
            client_secret: "client-secret".into(),
            refresh_token: encrypted,
            account_email: Some("ops@example.com".into()),
            extra: serde_json::json!({"root_folder_id": null}),
        };
        let json = serde_json::to_string(&wire).unwrap();
        // The cleartext refresh token must NEVER appear in the
        // serialised JSON.
        assert!(
            !json.contains("fake-refresh-token"),
            "cleartext leaked into config_json: {json}"
        );

        let parsed: CloudConfigJson = serde_json::from_str(&json).unwrap();
        let cfg = parsed.decrypt(secret).unwrap();
        assert_eq!(cfg.refresh_token.expose_secret(), "fake-refresh-token");
        assert_eq!(cfg.client_id, "client-id");
        assert_eq!(cfg.account_email.as_deref(), Some("ops@example.com"));
    }

    #[test]
    fn wrong_admin_secret_surfaces_as_auth_error() {
        let encrypted = token_crypto::encrypt("real-secret", "fake-refresh-token").unwrap();
        let wire = CloudConfigJson {
            client_id: "c".into(),
            client_secret: "s".into(),
            refresh_token: encrypted,
            account_email: None,
            extra: serde_json::Value::Null,
        };
        let err = wire.decrypt("wrong-secret").unwrap_err();
        match err {
            BackendError::Auth(msg) => {
                assert!(
                    !msg.contains("fake-refresh-token"),
                    "Auth error leaked plaintext: {msg}"
                );
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn cloud_config_debug_redacts_secrets() {
        let secret = "k";
        let enc = token_crypto::encrypt(secret, "TOPSECRET").unwrap();
        let cfg = CloudConfigJson {
            client_id: "cid".into(),
            client_secret: "TOPSECRET_CLIENT".into(),
            refresh_token: enc,
            account_email: None,
            extra: serde_json::Value::Null,
        }
        .decrypt(secret)
        .unwrap();
        let s = format!("{cfg:?}");
        assert!(!s.contains("TOPSECRET"), "leaked refresh token: {s}");
        assert!(!s.contains("TOPSECRET_CLIENT"), "leaked client secret: {s}");
        assert!(s.contains("<redacted>"));
    }
}
