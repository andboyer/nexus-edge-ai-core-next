//! HS256 JWT bearer middleware for admin-only API writes
//! (M2.2 Phase 2 step 12).
//!
//! ## Why this exists
//!
//! Phase 2 lets operators register cloud cold backends (Drive,
//! OneDrive). That registration **carries an OAuth refresh token**
//! — a credential as sensitive as the operator's cloud password.
//! The OAuth dance itself runs in this same binary; see the
//! `start_oauth` / `oauth_callback` / `oauth_status` handlers in
//! `api.rs` and the pending-session cache in `oauth_sessions.rs`.
//! The refresh token is encrypted with
//! [`nexus_storage::token_crypto`] before it ever reaches the
//! database. The HS256 bearer described here gates the *control
//! plane* (start/status, plus every other admin write) so that
//! anyone who can reach the engine's TCP port from off-host cannot
//! kick off an OAuth flow with their own client credentials and
//! redirect URI.
//!
//! Any **future** external admin caller — for example a
//! customer-supplied management plane that wants to script
//! backend rotation — uses the same HS256 gate by signing requests
//! with the shared admin secret.
//!
//! ## The contract
//!
//! * **Secret source:** `auth.admin_secret_path` in the engine
//!   TOML points at a JSON file (`{"secret":"..."}`). The same
//!   file feeds the AES-256-GCM refresh-token KDF in
//!   `nexus-storage`. Operators on a single-box deployment can
//!   leave it unset; the loopback fallback below kicks in.
//! * **Algorithm:** HS256 only. We never accept `alg=none` (the
//!   classic JWT footgun) and we never accept RS*/ES*/EdDSA — the
//!   verifier hard-pins to `Algorithm::HS256`.
//! * **Claims required:** `exp` (always validated by
//!   jsonwebtoken). Recommended but not enforced: `sub`, `iat`,
//!   `aud`. Leeway is 30s for clock-skew tolerance.
//! * **Gated routes:** every write (`PUT`/`POST`/`DELETE`/`PATCH`)
//!   under `/api/v1/admin/*` PLUS the OAuth `start` and `status`
//!   routes. The OAuth `callback` route is intentionally **un**gated
//!   because it's hit by the provider's redirect (not the operator);
//!   it's CSRF-protected by the opaque `state` token issued by
//!   `start`. GET requests for diagnostics stay open. Per
//!   plan-of-record §380 (Open Question 4), the gate covers all
//!   admin writes, not just cold-backend ones.
//! * **Fallback when no secret configured:** the engine falls back
//!   to "loopback peer is implicitly trusted" + the
//!   `NEXUS_ADMIN_BEARER_ALLOW_REMOTE=1` env override. This lets
//!   single-box dev deployments work with `curl localhost` without
//!   provisioning a shared secret, while still failing closed on
//!   a `0.0.0.0` bind by default.
//!
//! ## Threat model
//!
//! * **In scope:** anyone who can reach the engine's TCP port from
//!   off-host and tries to POST forged admin requests.
//! * **Out of scope:** anyone with shell on the engine host (they
//!   can read the secret file directly), TLS termination
//!   (handled upstream), replay across long time windows (`exp`
//!   bounds replay; nonce-based replay protection is a Phase-N
//!   follow-up).
//!
//! ## What this module deliberately does NOT do
//!
//! * **No JWT issuance.** The engine only verifies. Issuing
//!   requires the same secret on the engine side, which we don't
//!   want — secret access is asymmetric on purpose so the engine
//!   has no "what would I sign with this" surface to attack.
//!   External callers mint their own tokens against the shared
//!   secret.
//! * **No key rotation.** Same as `token_crypto` — secret_path is
//!   versioned externally; a future rotation tool re-signs.
//! * **No JWKS / asymmetric keys.** HS256 is enough for a single
//!   admin process. Asymmetric (RS256/ES256) lands if/when there
//!   are multiple distinct signers.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

/// Env-var escape hatch: when set to exactly `"1"`, the engine
/// accepts admin writes from non-loopback peers even when no
/// admin secret is configured. Intended for `cargo run`-style
/// developer setups where the engine binds `0.0.0.0:8089` for
/// network camera access but the operator has not yet provisioned
/// a shared admin secret. Production deployments should set
/// `auth.admin_secret_path` instead.
pub const ALLOW_REMOTE_ENV: &str = "NEXUS_ADMIN_BEARER_ALLOW_REMOTE";

/// Clock-skew tolerance for `exp` validation. 30 s is the same
/// figure most major JWT libraries default to; external admin
/// callers and the engine sync via the host clock so this is
/// plenty.
const LEEWAY_SECS: u64 = 30;

/// JWT claims the engine extracts from a valid bearer. We only
/// require `exp` — `sub` is captured for audit logging but
/// optional. Additional claims sent by external admin callers
/// are ignored silently (forward-compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminClaims {
    /// Expiry in seconds-since-epoch. jsonwebtoken validates this
    /// against the current clock + leeway during `decode()` —
    /// we don't need to re-check.
    pub exp: u64,
    /// Subject. External admin callers set this to a stable
    /// principal name (e.g. `"admin-tool"`). Surfaced into audit
    /// logs but not used for authorization decisions in this PR.
    #[serde(default)]
    pub sub: Option<String>,
    /// Issued-at, seconds-since-epoch. Optional; included for
    /// audit logging.
    #[serde(default)]
    pub iat: Option<u64>,
}

/// Reasons admin auth can reject a request. All variants render
/// as `401 Unauthorized` with a JSON `{"error":...}` body — we
/// deliberately do NOT distinguish "missing header" from "bad
/// signature" in the response body to avoid leaking which part
/// of the gate failed.
#[derive(Debug)]
pub enum AdminAuthError {
    /// No `Authorization` header (or it wasn't `Bearer ...`), and
    /// the fallback path (loopback / `ALLOW_REMOTE_ENV`) didn't
    /// apply.
    Missing,
    /// `Authorization` header present but the token did not
    /// verify (wrong signature, expired, wrong algorithm, or
    /// claims couldn't be parsed).
    Invalid,
}

impl IntoResponse for AdminAuthError {
    fn into_response(self) -> Response {
        // Single error body regardless of variant — see comment
        // on the enum. The variant lives in tracing logs only.
        let body = serde_json::json!({
            "error": "admin authentication required"
        });
        match self {
            AdminAuthError::Missing => {
                tracing::warn!("admin write rejected: no valid bearer + not loopback");
            }
            AdminAuthError::Invalid => {
                tracing::warn!("admin write rejected: invalid bearer token");
            }
        }
        (StatusCode::UNAUTHORIZED, Json(body)).into_response()
    }
}

/// Shared state for the admin-auth middleware. Constructed once
/// at engine boot and cloned cheaply into each request via
/// `axum::middleware::from_fn_with_state(Arc::new(...))`.
///
/// `key` is `None` when no `auth.admin_secret_path` is configured;
/// in that case the middleware falls back to dev-token-or-loopback
/// rules. `allow_remote` is a snapshot of `ALLOW_REMOTE_ENV` at
/// boot — we read it once so a runtime `unsetenv` doesn't change
/// behaviour mid-flight.
pub struct AdminAuthState {
    key: Option<DecodingKey>,
    /// Raw admin secret string — same material the JWT decoder
    /// uses, kept around for `nexus_storage::token_crypto::encrypt`
    /// / `decrypt`. These crypto paths are logically orthogonal to
    /// JWT verification: `token_crypto` derives an HKDF subkey
    /// with a different `info` string (`"oauth-refresh-token"`),
    /// so reusing the same shared secret with external admin
    /// callers is safe per RFC 5869 §2.2 (different `info` →
    /// independent key).
    /// Only `None` when no `auth.admin_secret_path` is configured;
    /// cloud-backend admin writes then fail closed.
    secret: Option<String>,
    /// Plain-string bearer for `auth.mode = "dev_token"` deployments.
    /// Auto-provisioned by `auth_bootstrap::apply` on first boot and
    /// pushed into `cfg.auth.dev_token`. When `Some(_)` the middleware
    /// accepts `Authorization: Bearer <dev_token>` from ANY peer
    /// (including remote ones), bypassing the loopback-only fallback.
    /// Compared constant-time. `None` outside DevToken mode.
    dev_token: Option<String>,
    allow_remote: bool,
}

impl AdminAuthState {
    /// Build from the engine's `AuthConfig`. Reads the secret
    /// file synchronously (at boot — non-hot path) and snapshots
    /// the allow-remote env var.
    ///
    /// Returns `Err` only when the file path is set but
    /// unreadable / unparseable; "no file configured" is the
    /// expected state for LAN-only deployments and is NOT an
    /// error.
    pub fn from_config(cfg: &nexus_config::AuthConfig) -> anyhow::Result<Self> {
        let secret = match &cfg.admin_secret_path {
            Some(p) => Some(load_secret_string(p)?),
            None => None,
        };
        let key = secret
            .as_deref()
            .map(|s| DecodingKey::from_secret(s.as_bytes()));
        // `cfg.dev_token` is populated by `auth_bootstrap::apply`
        // when `auth.mode = DevToken` (and left None otherwise),
        // so this clone is also the gate that decides whether the
        // bearer path below is active at all.
        let dev_token = cfg.dev_token.clone();
        let allow_remote = std::env::var(ALLOW_REMOTE_ENV).as_deref() == Ok("1");
        Ok(Self {
            key,
            secret,
            dev_token,
            allow_remote,
        })
    }

    /// Exposed for the cloud-backend write path. Cloud admin writes
    /// (`PUT /api/v1/admin/storage/backends/:handle` with
    /// `kind=gdrive|onedrive`) reject if this is `None` because the
    /// refresh token must be encrypted before it lands in SQLite.
    pub fn admin_secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    /// Test-only constructor — used by the integration tests to
    /// avoid touching the filesystem. Leaves `dev_token` unset;
    /// use [`Self::from_parts`] when a test exercises the
    /// DevToken-mode bearer path.
    #[cfg(test)]
    pub fn from_secret_bytes(secret: Option<&[u8]>, allow_remote: bool) -> Self {
        let secret_str = secret.map(|b| String::from_utf8_lossy(b).into_owned());
        Self {
            key: secret.map(DecodingKey::from_secret),
            secret: secret_str,
            dev_token: None,
            allow_remote,
        }
    }

    /// Test-only constructor with explicit `dev_token`. Same
    /// semantics as [`Self::from_secret_bytes`] otherwise.
    #[cfg(test)]
    pub fn from_parts(
        secret: Option<&[u8]>,
        dev_token: Option<&str>,
        allow_remote: bool,
    ) -> Self {
        let secret_str = secret.map(|b| String::from_utf8_lossy(b).into_owned());
        Self {
            key: secret.map(DecodingKey::from_secret),
            secret: secret_str,
            dev_token: dev_token.map(str::to_owned),
            allow_remote,
        }
    }
}

/// Read the admin secret string from `path`. Accepts EITHER a JSON
/// object `{"secret":"..."}` OR a plain text file whose contents
/// ARE the secret — strips trailing whitespace. Both layouts
/// coexist because operator-managed secrets ship in different
/// tools; the engine doesn't care which one it gets.
fn load_secret_string(path: &Path) -> anyhow::Result<String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read admin secret from {}: {e}", path.display()))?;
    let secret = if raw.trim_start().starts_with('{') {
        // Looks like JSON.
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parse admin secret JSON: {e}"))?;
        v.get("secret")
            .and_then(|s| s.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!("admin secret JSON is missing the \"secret\" string field")
            })?
            .to_string()
    } else {
        raw.trim().to_string()
    };
    if secret.is_empty() {
        return Err(anyhow::anyhow!(
            "admin secret at {} is empty",
            path.display()
        ));
    }
    Ok(secret)
}

/// True when the connecting peer is on the host's loopback. The
/// engine doesn't need to gate localhost-only deployments behind
/// an admin secret — by definition anyone who can reach 127.0.0.1
/// already has a shell on the box.
fn peer_is_loopback(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Extract the bearer token from an `Authorization: Bearer ...`
/// header, if present. Case-insensitive on the scheme; trims a
/// single space between scheme and token (no other whitespace
/// allowed — RFC 6750 §2.1).
fn extract_bearer(req: &Request) -> Option<&str> {
    let header = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = header.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty() {
        Some(token)
    } else {
        None
    }
}

/// Decode + validate a bearer token against the configured HS256
/// key. Caller verifies key presence; this function panics on
/// `None` because it's only ever called from the middleware path
/// that already checked.
fn verify_token(token: &str, key: &DecodingKey) -> Result<AdminClaims, AdminAuthError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = LEEWAY_SECS;
    // Don't require specific iss / aud — external admin callers
    // may evolve their claims layout and we want forward-compat.
    // exp is validated by default and we don't disable that.
    jsonwebtoken::decode::<AdminClaims>(token, key, &validation)
        .map(|d| d.claims)
        .map_err(|e| {
            tracing::debug!(error = %e, "JWT verification failed");
            AdminAuthError::Invalid
        })
}

/// Axum middleware applied to admin write routes. Lives at
/// `crates/nexus-engine/src/api.rs` wiring under
/// `axum::middleware::from_fn_with_state(state, admin_auth_layer)`.
///
/// Decision matrix (top wins):
///
/// 1. Valid HS256 JWT in `Authorization: Bearer ...` header → allow.
/// 2. No JWT (or bad JWT) AND secret IS configured → 401.
///    (Once a secret is configured, the bearer is mandatory —
///    no loopback bypass. Plan-of-record §380.)
/// 3. DevToken mode (`auth.mode = "dev_token"`, secret NOT
///    configured) AND bearer matches `cfg.auth.dev_token` → allow.
///    Match is constant-time so timing differences don't leak
///    which prefix matched.
/// 4. DevToken mode AND bearer present but doesn't match → 401.
///    Don't fall through to the loopback bypass: a wrong bearer
///    from any peer is an attack signal, not a typo by the local
///    operator.
/// 5. No JWT/dev_token bearer AND peer is loopback → allow.
///    (Loopback-only callers — local curl, cron — stay friction-
///    less even in DevToken mode.)
/// 6. No secret/dev_token bearer match AND `NEXUS_ADMIN_BEARER_ALLOW_REMOTE=1` → allow.
/// 7. Otherwise → 401.
pub async fn admin_auth_layer(
    State(state): State<Arc<AdminAuthState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Result<Response, AdminAuthError> {
    // M6 Phase 4 Step 4.1 — every successful auth path injects a
    // `SessionContext` extension that downstream handlers extract
    // via `req.extensions().get::<SessionContext>()` to populate
    // the actor columns of every `audit_log` row. The HS256 path
    // captures `sub` (when present) as the actor label; the
    // loopback bypass + remote-bypass paths tag a synthetic actor
    // so the audit row still reflects the *mode* the engine was
    // configured in.
    use crate::auth::require_role::SessionContext;
    use nexus_types::Role;

    // Path 1 + 2 (secret configured): JWT-or-bust.
    if let Some(key) = &state.key {
        let token = extract_bearer(&req).ok_or(AdminAuthError::Missing)?;
        let claims = verify_token(token, key)?;
        tracing::info!(
            sub = claims.sub.as_deref().unwrap_or("<none>"),
            exp = claims.exp,
            path = %req.uri().path(),
            method = %req.method(),
            "admin write authorised by HS256 bearer"
        );
        let ctx = SessionContext {
            user_id: 0,
            role: Role::Admin,
            jti: claims.sub.clone().unwrap_or_else(|| "legacy".to_string()),
            is_legacy_admin: true,
        };
        req.extensions_mut().insert(ctx);
        return Ok(next.run(req).await);
    }

    // Path 3 + 4 (DevToken mode, secret NOT configured): plain-
    // string bearer compared constant-time. A bearer that's
    // present but mismatched is a hard 401 — we deliberately do
    // NOT fall through to loopback bypass for that case.
    if let Some(expected) = state.dev_token.as_deref() {
        if let Some(provided) = extract_bearer(&req) {
            if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
                tracing::info!(
                    peer = %peer,
                    path = %req.uri().path(),
                    method = %req.method(),
                    "admin write authorised by dev_token bearer"
                );
                let ctx = SessionContext {
                    user_id: 0,
                    role: Role::Admin,
                    jti: "dev-token".to_string(),
                    is_legacy_admin: true,
                };
                req.extensions_mut().insert(ctx);
                return Ok(next.run(req).await);
            }
            tracing::warn!(peer = %peer, "admin write rejected: dev_token mismatch");
            return Err(AdminAuthError::Invalid);
        }
        // No bearer in DevToken mode — fall through to loopback
        // / allow_remote bypass below so local curl still works.
    }

    // Path 5 + 6 + 7 (no JWT, no dev_token match): loopback or escape-hatch.
    if peer_is_loopback(&peer) {
        tracing::debug!(peer = %peer, "admin write allowed: loopback peer (no secret configured)");
        let ctx = SessionContext {
            user_id: 0,
            role: Role::Admin,
            jti: "loopback".to_string(),
            is_legacy_admin: true,
        };
        req.extensions_mut().insert(ctx);
        return Ok(next.run(req).await);
    }
    if state.allow_remote {
        tracing::warn!(
            peer = %peer,
            "admin write allowed via NEXUS_ADMIN_BEARER_ALLOW_REMOTE=1 — production deployments should configure auth.admin_secret_path"
        );
        let ctx = SessionContext {
            user_id: 0,
            role: Role::Admin,
            jti: "allow-remote".to_string(),
            is_legacy_admin: true,
        };
        req.extensions_mut().insert(ctx);
        return Ok(next.run(req).await);
    }
    Err(AdminAuthError::Missing)
}

/// Constant-time byte-slice equality. Returns `false` immediately
/// on length mismatch (length is not a secret), then ORs every
/// byte XOR so the total comparison time is independent of WHERE
/// the first differing byte sits. Used to compare dev-token
/// bearers without leaking the secret prefix via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// `admin_auth_layer` attaches as a tower layer via
// `axum::middleware::from_fn_with_state` — see the call site in
// `api::router` for the wiring.

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use axum::routing::put;
    use axum::Router;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt; // for oneshot

    const TEST_SECRET: &[u8] = b"shared-with-nexus-admin-do-not-leak";

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn sign_jwt(secret: &[u8], exp_offset_secs: i64, sub: Option<&str>) -> String {
        #[derive(Serialize)]
        struct Claims<'a> {
            exp: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            sub: Option<&'a str>,
            iat: u64,
        }
        let now = now();
        let exp = (now as i64 + exp_offset_secs).max(0) as u64;
        let claims = Claims { exp, sub, iat: now };
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    /// Build a router with a single PUT /protected handler gated
    /// by the middleware under test. Returns 204 No Content when
    /// allowed.
    fn make_app(state: AdminAuthState) -> Router {
        let state = Arc::new(state);
        Router::new()
            .route("/protected", put(|| async { StatusCode::NO_CONTENT }))
            .route_layer(axum::middleware::from_fn_with_state(
                state,
                admin_auth_layer,
            ))
    }

    fn req_with_peer(headers: &[(&str, &str)], peer: SocketAddr) -> Request<Body> {
        let mut req = Request::builder().method(Method::PUT).uri("/protected");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let mut req = req.body(Body::empty()).unwrap();
        // Inject ConnectInfo<SocketAddr> the same way axum does
        // when the server is built with
        // `into_make_service_with_connect_info::<SocketAddr>()`.
        req.extensions_mut().insert(ConnectInfo(peer));
        req
    }

    fn loopback() -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, 8089))
    }

    fn loopback_v6() -> SocketAddr {
        SocketAddr::from((Ipv6Addr::LOCALHOST, 8089))
    }

    fn remote() -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(10, 0, 0, 5), 51234))
    }

    #[tokio::test]
    async fn valid_jwt_allows_write() {
        let app = make_app(AdminAuthState::from_secret_bytes(Some(TEST_SECRET), false));
        let token = sign_jwt(TEST_SECRET, 60, Some("nexus-admin"));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", &format!("Bearer {token}"))],
                remote(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn expired_jwt_is_rejected() {
        let app = make_app(AdminAuthState::from_secret_bytes(Some(TEST_SECRET), false));
        // exp = now - 5min, well beyond the 30s leeway.
        let token = sign_jwt(TEST_SECRET, -300, Some("nexus-admin"));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", &format!("Bearer {token}"))],
                loopback(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_signature_is_rejected() {
        let app = make_app(AdminAuthState::from_secret_bytes(Some(TEST_SECRET), false));
        let token = sign_jwt(b"different-secret", 60, Some("attacker"));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", &format!("Bearer {token}"))],
                loopback(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn alg_none_is_rejected() {
        // Manually craft an `alg=none` JWT — the classic JWT
        // footgun. jsonwebtoken refuses to decode it because the
        // verifier is pinned to HS256.
        use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
        use base64::Engine as _;
        let header = B64URL.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = B64URL.encode(format!(r#"{{"exp":{}}}"#, now() + 60).as_bytes());
        let token = format!("{header}.{payload}.");
        let app = make_app(AdminAuthState::from_secret_bytes(Some(TEST_SECRET), false));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", &format!("Bearer {token}"))],
                loopback(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn malformed_authorization_header_is_rejected() {
        let app = make_app(AdminAuthState::from_secret_bytes(Some(TEST_SECRET), false));
        // Wrong scheme.
        let res = app
            .clone()
            .oneshot(req_with_peer(
                &[("authorization", "Basic dXNlcjpwYXNz")],
                loopback(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // No scheme.
        let res = app
            .clone()
            .oneshot(req_with_peer(&[("authorization", "abcdef")], loopback()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // Empty token.
        let res = app
            .oneshot(req_with_peer(&[("authorization", "Bearer ")], loopback()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn no_jwt_with_secret_configured_is_rejected_even_on_loopback() {
        // Plan-of-record §380: once the secret is configured the
        // bearer is mandatory — no loopback bypass.
        let app = make_app(AdminAuthState::from_secret_bytes(Some(TEST_SECRET), false));
        let res = app.oneshot(req_with_peer(&[], loopback())).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn no_secret_loopback_v4_is_allowed() {
        let app = make_app(AdminAuthState::from_secret_bytes(None, false));
        let res = app.oneshot(req_with_peer(&[], loopback())).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn no_secret_loopback_v6_is_allowed() {
        let app = make_app(AdminAuthState::from_secret_bytes(None, false));
        let res = app
            .oneshot(req_with_peer(&[], loopback_v6()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn no_secret_remote_without_env_is_rejected() {
        let app = make_app(AdminAuthState::from_secret_bytes(None, false));
        let res = app.oneshot(req_with_peer(&[], remote())).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn no_secret_remote_with_allow_remote_env_is_allowed() {
        let app = make_app(AdminAuthState::from_secret_bytes(None, true));
        let res = app.oneshot(req_with_peer(&[], remote())).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    // ------------------------------------------------------------------
    // DevToken-mode tests. With `auth.mode = "dev_token"` the engine
    // auto-provisions a 32-byte URL-safe bearer (see auth_bootstrap.rs)
    // and pushes it into `cfg.auth.dev_token`. The middleware below
    // must accept that exact bearer from ANY peer (including the LAN
    // bind that motivates the mode in the first place).
    // ------------------------------------------------------------------

    const TEST_DEV_TOKEN: &str = "TEST_DEV_TOKEN_43_chars__padded__padded__pad";

    #[tokio::test]
    async fn dev_token_bearer_match_allows_remote_write() {
        let app = make_app(AdminAuthState::from_parts(None, Some(TEST_DEV_TOKEN), false));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", &format!("Bearer {TEST_DEV_TOKEN}"))],
                remote(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn dev_token_bearer_match_allows_loopback_write() {
        let app = make_app(AdminAuthState::from_parts(None, Some(TEST_DEV_TOKEN), false));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", &format!("Bearer {TEST_DEV_TOKEN}"))],
                loopback(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn dev_token_bearer_mismatch_returns_401_no_loopback_fallthrough() {
        // Wrong bearer from a LOOPBACK peer must still be a 401 —
        // a non-matching bearer is an attack signal regardless of
        // where the connection came from.
        let app = make_app(AdminAuthState::from_parts(None, Some(TEST_DEV_TOKEN), false));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", "Bearer not-the-real-token-not-the-real-toke")],
                loopback(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn dev_token_bearer_mismatch_returns_401_remote() {
        let app = make_app(AdminAuthState::from_parts(None, Some(TEST_DEV_TOKEN), false));
        let res = app
            .oneshot(req_with_peer(
                &[("authorization", "Bearer attacker-guess-attacker-guess-attacker")],
                remote(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn dev_token_no_bearer_loopback_still_allowed() {
        // Local curl shouldn't have to discover the dev_token —
        // loopback bypass continues to apply when no bearer is
        // sent at all.
        let app = make_app(AdminAuthState::from_parts(None, Some(TEST_DEV_TOKEN), false));
        let res = app.oneshot(req_with_peer(&[], loopback())).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn dev_token_no_bearer_remote_is_rejected() {
        let app = make_app(AdminAuthState::from_parts(None, Some(TEST_DEV_TOKEN), false));
        let res = app.oneshot(req_with_peer(&[], remote())).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn constant_time_eq_basic_cases() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn load_secret_from_json_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-auth.json");
        std::fs::write(&path, r#"{"secret":"hunter2","other":"ignored"}"#).unwrap();
        let secret = load_secret_string(&path).unwrap();
        assert_eq!(secret, "hunter2");
        let key = DecodingKey::from_secret(secret.as_bytes());
        // Round-trip a JWT through it to confirm the secret bytes
        // round-tripped correctly.
        let token = encode(
            &Header::new(Algorithm::HS256),
            &serde_json::json!({"exp": now() + 60}),
            &EncodingKey::from_secret(b"hunter2"),
        )
        .unwrap();
        let mut v = Validation::new(Algorithm::HS256);
        v.leeway = 30;
        v.required_spec_claims.clear();
        v.required_spec_claims.insert("exp".to_string());
        jsonwebtoken::decode::<serde_json::Value>(&token, &key, &v).unwrap();
    }

    #[test]
    fn load_secret_from_plain_text_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        std::fs::write(&path, "  hunter2\n").unwrap();
        let secret = load_secret_string(&path).unwrap();
        let key = DecodingKey::from_secret(secret.as_bytes());
        let token = encode(
            &Header::new(Algorithm::HS256),
            &serde_json::json!({"exp": now() + 60}),
            &EncodingKey::from_secret(b"hunter2"),
        )
        .unwrap();
        let mut v = Validation::new(Algorithm::HS256);
        v.leeway = 30;
        jsonwebtoken::decode::<serde_json::Value>(&token, &key, &v).unwrap();
    }

    #[test]
    fn empty_secret_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, "   \n  ").unwrap();
        let err = match load_secret_string(&path) {
            Ok(_) => panic!("expected error on empty secret file"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn malformed_secret_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, r#"{"not_secret":"x"}"#).unwrap();
        let err = match load_secret_string(&path) {
            Ok(_) => panic!("expected error on malformed secret JSON"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("secret"));
    }
}
