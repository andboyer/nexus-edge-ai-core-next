// M6 Phase 3 Step 3.1 — OIDC discovery + JWKS validation client.
//
// Owns three jobs:
//
//   1. **Discovery.** At construction time we GET
//      `<issuer>/.well-known/openid-configuration`, parse the
//      minimal subset of OIDC discovery metadata we need
//      (`issuer`, `authorization_endpoint`, `token_endpoint`,
//      `jwks_uri`), and stash it inside an `ArcSwap` so the
//      auth-code flow handler (Step 3.3) can read the current
//      value lock-free.
//
//   2. **JWKS cache.** We then GET the discovery's `jwks_uri`,
//      parse the JWK set into a `kid → DecodingKey` map, and
//      stash it inside a second `ArcSwap`. A background task
//      re-fetches both discovery + JWKS every
//      `OidcClient::REFRESH_INTERVAL` (default 1 hour) so a
//      key rotation at the IdP is picked up without a
//      restart.
//
//   3. **ID-token validation.** `verify_id_token(jwt)` decodes
//      the JWT, picks the right key from the JWKS by `kid`,
//      verifies the signature, AND validates `iss` (must equal
//      the configured issuer), `aud` (must equal the
//      configured audience), `exp` + `nbf` (against the
//      caller-supplied clock for deterministic tests). Returns
//      a `VerifiedIdToken` carrying the parsed claims so the
//      Step 3.2 role mapper can lift them straight off.
//
// What this module deliberately does NOT do:
//
//   * Run the auth-code + PKCE flow. That handler lands in
//     Step 3.3 and consumes `OidcClient::token_endpoint()` +
//     `verify_id_token()`.
//   * Pull groups / roles from claims. Step 3.2 owns claim →
//     `Role` mapping based on `auth.oidc.role_claims` +
//     `role_map`.
//   * Cache nonce + state for CSRF. That's also Step 3.3 — the
//     mapping from `state` to the in-flight login session
//     lives next to the handler that hands them out.
//
// Algorithm support: RS256 / RS384 / RS512 / ES256 / ES384.
// Every M6-supported IdP (Authentik, Keycloak, Azure AD, Okta,
// Google Workspace) defaults to RS256; the ES* variants are
// here so an operator who's intentionally rotated their IdP to
// EC keys doesn't get a "unsupported algorithm" surprise.
// HS256 is rejected because shared-secret OIDC is operationally
// indistinguishable from `auth.mode = dev_token` and would
// undermine the whole point of M6.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

use nexus_config::OidcConfig;

/// How often the background task re-fetches discovery + JWKS.
/// One hour matches the M6 doc; it's the same cadence
/// Authentik / Keycloak / Azure AD recommend for their
/// federated relying parties.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3600);

/// HTTP timeout for both the discovery and JWKS fetches.
/// Short enough that a misconfigured issuer URL doesn't stall
/// engine startup behind a stuck TLS handshake, long enough
/// that a slow IdP behind a CDN still wins.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Wire types — only the fields we actually consume. The
// `serde(deny_unknown_fields)` is deliberately NOT set: real
// IdPs ship dozens of extra discovery fields and we don't want
// to break on them.
// ---------------------------------------------------------------------------

/// Subset of the OpenID Connect Discovery 1.0 metadata
/// document. Spec: <https://openid.net/specs/openid-connect-discovery-1_0.html#ProviderMetadata>
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiscoveryDocument {
    /// MUST exactly match the issuer URL the operator
    /// configured. Validated on first fetch — a mismatch is an
    /// outright IdP-misconfiguration error.
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    /// Optional — the userinfo endpoint. Not required by our
    /// flow today (Step 3.2 maps roles from the ID token's
    /// claims, not from userinfo), but stash it so a future
    /// step that wants the full profile can find it.
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
    /// Optional — populated by every IdP we care about; the
    /// auth-code-with-PKCE flow needs `S256` in this list.
    #[serde(default)]
    pub code_challenge_methods_supported: Vec<String>,
}

/// A single JSON Web Key. We only consume RSA + EC keys; any
/// `kty` we don't recognise is silently dropped from the cache
/// (with a `tracing::warn!`) so an IdP that adds new key types
/// in a JWKS rotation doesn't kill an otherwise-healthy refresh.
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    /// Key ID. The JWT header's `kid` MUST match this for us
    /// to consider the key as a candidate.
    pub kid: Option<String>,
    /// `RSA`, `EC`, `oct` (rejected — see module comment), or
    /// anything else we don't support.
    pub kty: String,
    /// `sig` for signature verification, `enc` for encryption.
    /// We only want `sig`.
    #[serde(default)]
    pub r#use: Option<String>,
    /// Hint at the algorithm the key is intended for. Some
    /// IdPs (e.g. Microsoft) omit this; in that case we pick a
    /// sane default per `kty`.
    pub alg: Option<String>,
    // -- RSA --
    /// Modulus (base64url, no padding).
    pub n: Option<String>,
    /// Public exponent (base64url, no padding).
    pub e: Option<String>,
    // -- EC --
    /// Curve name (`P-256`, `P-384`).
    pub crv: Option<String>,
    pub x: Option<String>,
    pub y: Option<String>,
}

/// The set returned by an OIDC `jwks_uri`. Wraps a list of
/// raw [`Jwk`] entries; the cache derives a `kid → DecodingKey`
/// map from this so verification is O(1).
#[derive(Debug, Clone, Deserialize)]
pub struct JwksDocument {
    pub keys: Vec<Jwk>,
}

/// Parsed JWK set in the form the validator actually needs:
/// a map keyed by `kid` and a fallback for keys missing `kid`
/// (some IdPs publish a single-key JWKS without one — we accept
/// that by picking the lone key if exactly one is present).
struct PreparedJwks {
    by_kid: HashMap<String, PreparedKey>,
    /// Used iff the JWT header has no `kid` AND the set
    /// contains exactly one usable key. Matches the
    /// jsonwebtoken-recipe most IdPs link to.
    sole_key: Option<PreparedKey>,
}

impl std::fmt::Debug for PreparedJwks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `DecodingKey` is opaque — just summarise.
        f.debug_struct("PreparedJwks")
            .field("by_kid_count", &self.by_kid.len())
            .field("kids", &self.by_kid.keys().collect::<Vec<_>>())
            .field("has_sole_key", &self.sole_key.is_some())
            .finish()
    }
}

#[derive(Clone)]
struct PreparedKey {
    alg: Algorithm,
    key: DecodingKey,
}

// ---------------------------------------------------------------------------
// Error type.
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    /// Discovery or JWKS fetch failed at the HTTP / DNS / TLS
    /// layer.
    #[error("http: {0}")]
    Http(String),
    /// The fetched payload parsed but failed structural
    /// validation (e.g. `iss` mismatch, no usable keys, missing
    /// required endpoint).
    #[error("invalid_discovery: {0}")]
    InvalidDiscovery(String),
    /// The JWKS could be fetched and parsed, but contained no
    /// keys we can verify against (every key was a `kty` we
    /// don't support).
    #[error("no_usable_keys")]
    NoUsableKeys,
    /// JWT failed signature, claim, or expiry validation. The
    /// inner string is a stable error tag (`bad_signature`,
    /// `bad_iss`, `bad_aud`, `expired`, `not_yet_valid`,
    /// `unknown_kid`, `unsupported_alg`, `malformed`) so the
    /// caller can fold them into the constant `401
    /// invalid_credentials` body without losing the audit
    /// detail.
    #[error("invalid_id_token: {0}")]
    InvalidIdToken(&'static str),
}

// ---------------------------------------------------------------------------
// OidcClient.
// ---------------------------------------------------------------------------

/// Long-lived OIDC client. Holds the live discovery + JWKS in
/// `ArcSwap` so handler-side reads are lock-free; the
/// background refresh task swaps fresh copies in atomically.
pub struct OidcClient {
    cfg: OidcConfig,
    discovery: ArcSwap<DiscoveryDocument>,
    jwks: ArcSwap<PreparedJwks>,
    http: reqwest::Client,
}

impl std::fmt::Debug for OidcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcClient")
            .field("issuer", &self.cfg.issuer)
            .field("audience", &self.cfg.audience)
            .field("discovery", &*self.discovery.load())
            .field("jwks", &*self.jwks.load())
            .finish()
    }
}

impl OidcClient {
    /// Construct a client by performing one discovery + JWKS
    /// fetch. Fails fast if either request fails or the
    /// discovered `issuer` doesn't match the configured one
    /// (a mismatch is almost always a misconfigured IdP URL —
    /// e.g. trailing slash where there shouldn't be one).
    pub async fn discover(cfg: OidcConfig) -> Result<Self, OidcError> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| OidcError::Http(e.to_string()))?;

        let discovery = fetch_discovery(&http, &cfg).await?;
        let jwks_uri = cfg
            .jwks_uri
            .clone()
            .unwrap_or_else(|| discovery.jwks_uri.clone());
        let jwks = fetch_jwks(&http, &jwks_uri).await?;

        Ok(Self {
            cfg,
            discovery: ArcSwap::from_pointee(discovery),
            jwks: ArcSwap::from_pointee(jwks),
            http,
        })
    }

    /// Re-fetch discovery + JWKS and atomically swap them in.
    /// Errors are returned to the caller (typically the
    /// refresh-loop task) so they can log + retry without
    /// dropping the currently-loaded version.
    pub async fn refresh(&self) -> Result<(), OidcError> {
        let discovery = fetch_discovery(&self.http, &self.cfg).await?;
        let jwks_uri = self
            .cfg
            .jwks_uri
            .clone()
            .unwrap_or_else(|| discovery.jwks_uri.clone());
        let jwks = fetch_jwks(&self.http, &jwks_uri).await?;
        self.discovery.store(Arc::new(discovery));
        self.jwks.store(Arc::new(jwks));
        Ok(())
    }

    /// Verify an ID token end-to-end: signature over the
    /// current JWKS, `iss` exactly equals the configured
    /// issuer, `aud` exactly equals the configured audience,
    /// `exp > now`, `nbf <= now` if present. `now` is
    /// caller-supplied so the test suite is deterministic.
    pub fn verify_id_token(
        &self,
        jwt: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<VerifiedIdToken, OidcError> {
        let header =
            jsonwebtoken::decode_header(jwt).map_err(|_| OidcError::InvalidIdToken("malformed"))?;
        let alg = header.alg;
        if !is_supported_alg(alg) {
            return Err(OidcError::InvalidIdToken("unsupported_alg"));
        }

        let jwks = self.jwks.load();
        let key = match header.kid.as_deref() {
            Some(kid) => jwks
                .by_kid
                .get(kid)
                .ok_or(OidcError::InvalidIdToken("unknown_kid"))?,
            None => jwks
                .sole_key
                .as_ref()
                .ok_or(OidcError::InvalidIdToken("unknown_kid"))?,
        };
        if key.alg != alg {
            return Err(OidcError::InvalidIdToken("unsupported_alg"));
        }

        let mut validation = Validation::new(alg);
        validation.set_audience(&[self.cfg.audience.as_str()]);
        validation.set_issuer(&[self.cfg.issuer.as_str()]);
        // We do our own `exp` / `nbf` check against the caller's
        // `now` so tests can pin a specific clock. Disable
        // jsonwebtoken's own time gate to avoid the
        // SystemTime::now()-based race.
        validation.validate_exp = false;
        validation.validate_nbf = false;

        let data =
            jsonwebtoken::decode::<IdTokenClaims>(jwt, &key.key, &validation).map_err(|e| {
                use jsonwebtoken::errors::ErrorKind::*;
                match e.kind() {
                    InvalidSignature => OidcError::InvalidIdToken("bad_signature"),
                    InvalidIssuer => OidcError::InvalidIdToken("bad_iss"),
                    InvalidAudience => OidcError::InvalidIdToken("bad_aud"),
                    _ => OidcError::InvalidIdToken("malformed"),
                }
            })?;

        // Manual expiry check (see comment above).
        let now_ts = now.timestamp();
        if data.claims.exp <= now_ts {
            return Err(OidcError::InvalidIdToken("expired"));
        }
        if let Some(nbf) = data.claims.nbf {
            if nbf > now_ts {
                return Err(OidcError::InvalidIdToken("not_yet_valid"));
            }
        }

        Ok(VerifiedIdToken {
            claims: data.claims,
        })
    }

    /// Lock-free read of the current discovery document.
    /// Returned as `Arc<DiscoveryDocument>` so the caller can
    /// hold it across `.await` boundaries without blocking
    /// the refresh task.
    pub fn discovery(&self) -> Arc<DiscoveryDocument> {
        self.discovery.load_full()
    }

    /// Convenience: token endpoint, the only field the
    /// auth-code flow handler reads in the hot path.
    pub fn token_endpoint(&self) -> String {
        self.discovery.load().token_endpoint.clone()
    }

    /// Convenience: authorization endpoint, for building the
    /// redirect URL on the login page.
    pub fn authorization_endpoint(&self) -> String {
        self.discovery.load().authorization_endpoint.clone()
    }
}

/// Parsed claims after a successful `verify_id_token`. The
/// `extra` field carries every claim we don't explicitly model
/// (groups, roles, custom claims) so the Step 3.2 role mapper
/// can pluck them out by name without re-parsing the JWT.
#[derive(Debug, Clone)]
pub struct VerifiedIdToken {
    pub claims: IdTokenClaims,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdTokenClaims {
    pub iss: String,
    /// OIDC `aud` is documented as string-or-array. We always
    /// emit a single-element array via the helper; on parse,
    /// `jsonwebtoken` handles both shapes when `Validation`
    /// has a single audience configured.
    pub aud: serde_json::Value,
    pub sub: String,
    pub exp: i64,
    #[serde(default)]
    pub nbf: Option<i64>,
    #[serde(default)]
    pub iat: Option<i64>,
    /// Every other claim. The role-mapper reads `groups`,
    /// `roles`, and any custom-claim-name configured in
    /// `auth.oidc.role_claims` out of here.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Internals.
// ---------------------------------------------------------------------------

fn is_supported_alg(alg: Algorithm) -> bool {
    matches!(
        alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384
    )
}

/// Build the well-known discovery URL by appending the suffix
/// to the issuer. Per RFC 8414 / OIDC discovery spec the
/// suffix is always `/.well-known/openid-configuration`. We
/// trim a trailing slash on the issuer so both `https://idp/`
/// and `https://idp` work.
fn discovery_url(issuer: &str) -> String {
    let trimmed = issuer.trim_end_matches('/');
    format!("{trimmed}/.well-known/openid-configuration")
}

async fn fetch_discovery(
    http: &reqwest::Client,
    cfg: &OidcConfig,
) -> Result<DiscoveryDocument, OidcError> {
    let url = discovery_url(&cfg.issuer);
    let resp = http
        .get(&url)
        .send()
        .await
        .map_err(|e| OidcError::Http(format!("discovery GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(OidcError::Http(format!(
            "discovery GET {url}: HTTP {}",
            resp.status()
        )));
    }
    let body: DiscoveryDocument = resp
        .json()
        .await
        .map_err(|e| OidcError::Http(format!("discovery parse: {e}")))?;

    // Issuer-mismatch detection. Per OIDC spec the `issuer`
    // value in the discovery doc MUST exactly equal the URL
    // the relying party used to discover it. A mismatch usually
    // indicates a typo in the operator's config (most often a
    // trailing slash) — fail loud, do NOT silently rewrite.
    if normalise_issuer(&body.issuer) != normalise_issuer(&cfg.issuer) {
        return Err(OidcError::InvalidDiscovery(format!(
            "issuer mismatch: configured {:?}, discovery doc reports {:?}",
            cfg.issuer, body.issuer
        )));
    }
    if body.jwks_uri.is_empty() && cfg.jwks_uri.is_none() {
        return Err(OidcError::InvalidDiscovery(
            "no jwks_uri (neither in discovery doc nor config override)".into(),
        ));
    }
    Ok(body)
}

fn normalise_issuer(s: &str) -> &str {
    s.trim_end_matches('/')
}

async fn fetch_jwks(http: &reqwest::Client, jwks_uri: &str) -> Result<PreparedJwks, OidcError> {
    let resp = http
        .get(jwks_uri)
        .send()
        .await
        .map_err(|e| OidcError::Http(format!("jwks GET {jwks_uri}: {e}")))?;
    if !resp.status().is_success() {
        return Err(OidcError::Http(format!(
            "jwks GET {jwks_uri}: HTTP {}",
            resp.status()
        )));
    }
    let doc: JwksDocument = resp
        .json()
        .await
        .map_err(|e| OidcError::Http(format!("jwks parse: {e}")))?;
    prepare_jwks(doc)
}

fn prepare_jwks(doc: JwksDocument) -> Result<PreparedJwks, OidcError> {
    let mut by_kid: HashMap<String, PreparedKey> = HashMap::new();
    let mut anon_keys: Vec<PreparedKey> = Vec::new();

    for jwk in doc.keys {
        if jwk.r#use.as_deref() == Some("enc") {
            continue; // encryption keys are not signature candidates
        }
        let prepared = match prepare_one(&jwk) {
            Ok(p) => p,
            Err(reason) => {
                tracing::warn!(
                    target: "nexus::oidc",
                    kty = %jwk.kty,
                    kid = ?jwk.kid,
                    reason,
                    "dropping unusable JWK from JWKS cache"
                );
                continue;
            }
        };
        match jwk.kid {
            Some(kid) => {
                by_kid.insert(kid, prepared);
            }
            None => anon_keys.push(prepared),
        }
    }

    let sole_key = if by_kid.is_empty() && anon_keys.len() == 1 {
        anon_keys.into_iter().next()
    } else {
        None
    };

    if by_kid.is_empty() && sole_key.is_none() {
        return Err(OidcError::NoUsableKeys);
    }

    Ok(PreparedJwks { by_kid, sole_key })
}

fn prepare_one(jwk: &Jwk) -> Result<PreparedKey, &'static str> {
    match jwk.kty.as_str() {
        "RSA" => {
            let n = jwk.n.as_deref().ok_or("rsa: missing n")?;
            let e = jwk.e.as_deref().ok_or("rsa: missing e")?;
            let alg = match jwk.alg.as_deref() {
                Some("RS256") | None => Algorithm::RS256,
                Some("RS384") => Algorithm::RS384,
                Some("RS512") => Algorithm::RS512,
                Some(other) => {
                    tracing::warn!(target: "nexus::oidc", alg = other, "RSA JWK with unsupported alg");
                    return Err("rsa: unsupported alg");
                }
            };
            let key = DecodingKey::from_rsa_components(n, e).map_err(|_| "rsa: invalid n/e")?;
            Ok(PreparedKey { alg, key })
        }
        "EC" => {
            let x = jwk.x.as_deref().ok_or("ec: missing x")?;
            let y = jwk.y.as_deref().ok_or("ec: missing y")?;
            let crv = jwk.crv.as_deref().ok_or("ec: missing crv")?;
            let alg = match (jwk.alg.as_deref(), crv) {
                (Some("ES256"), "P-256") | (None, "P-256") => Algorithm::ES256,
                (Some("ES384"), "P-384") | (None, "P-384") => Algorithm::ES384,
                _ => return Err("ec: unsupported alg/crv pair"),
            };
            let key = DecodingKey::from_ec_components(x, y).map_err(|_| "ec: invalid x/y")?;
            Ok(PreparedKey { alg, key })
        }
        "oct" => Err("oct: shared-secret OIDC rejected by policy"),
        _ => Err("unknown kty"),
    }
}

// ---------------------------------------------------------------------------
// Background refresh task. Started by `OidcClient::spawn_refresh`
// in the engine boot path. Returns a `JoinHandle` so a graceful
// shutdown can cancel it.
// ---------------------------------------------------------------------------

impl OidcClient {
    /// Spawn the refresh loop. Caller keeps the returned
    /// `JoinHandle` if they want to abort on shutdown; ignoring
    /// it is fine for the long-running engine process.
    pub fn spawn_refresh(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let client = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REFRESH_INTERVAL).await;
                if let Err(e) = client.refresh().await {
                    tracing::warn!(
                        target: "nexus::oidc",
                        error = %e,
                        "OIDC discovery+jwks refresh failed; keeping previous cache"
                    );
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use jsonwebtoken::{EncodingKey, Header};
    use nexus_config::OidcRoleMap;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Test RSA keypair — embedded once, used by every test
    // that needs to sign an ID token. Generated via
    //   openssl genrsa -out test_oidc.pem 2048
    //   python3 -c '...' to derive JWK n/e components.
    const TEST_RSA_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC63EWXnT/woDT9
HNePuAgCo4esirxphgBb7SXqW1GiYxhg0OzjXipneZubBNJPBu8bfoV2nOD2gLYd
VG+UMXwFMQ+2Fe/coI+MEXpY6MDOgeFGQJlFlKsug21VqMVKEuXvZFGYpMQLMRZC
jCHL7HbnLRZ4hLjx8YoK6xi0MTwvTdx+hj9ADnLGIPyxxjSEVwwJ5vkIL2GNSmky
a8qwdRWX3AHLuzP1DbamtF9X5XJcUw4/xYLonsDv176qLiNRy6new7OnI/IEa7VU
mgAcjTjQEXKRhOR6UJBIgw/yI4EheFx4nQpEAapFxXS926Z4AK0MZVAa2p1in+Gc
vm10Tl1FAgMBAAECggEAHgGZO0uJJP7VA4BKLU4EG0hfVRnCP7/A9wISAJxBc64u
D/38zFYmwQ8KKc6QZxhZ9TRQAkgW5/6rP/J7KHT50WubdkX3ippN8tQrrXNS4taA
cmOVOVMyWfnI6OR9KfP28IZPhA+RW6q6RF10X+bNATqATUIi4NdEr5QpF2aAn6e+
nfEplRCgqRdbrGDKhK+DfBWYXXjeuH6iluaoesneHOKZkqBuIZdv5yquxI98Z2V7
SCC5FcG1cIQ+GyZ5ZliL+wUIVkFUrFUIy66VFIVoDUVc+7+5TQwXu798nwXFWMha
gSWUH48nsu8I+rAG8vL4bXXwbADsMqKtE8nseUAcAQKBgQDwX0mOcL865H8LQRdd
VKax1UNVVNXmZq3IMN5SQZpwocFspbroeUQGTHawtlICtonqrEgV4pKaT4R9POts
8URO+wWNXzcJ6/G2YF1X7TOlD/UGZNR+HzwuxoEcVn2BrwBktxh/8s5n8ysSggkR
cX5ShJSz6Ylh/7MToAp5TlucBQKBgQDHAlgLPvUsKdRsiGnIjh5+UHDvOqpuXwN/
sQ3TeNGrtJpHkcMrz7iHCIrLjE80Uiht4BDnwfdAKZOa9XfsQxiqr/Qq3xdbKahI
sOtkSlbWudlsok8rkSUx7mucqCRiJV1TFeBDvvlJFTyWWGebqrUQFZ9tV+nLsfYO
JWY0GGjAQQKBgFtcDCwMZFJw+GyBn+zEONtMcFwlc2svyYtWL5nZ/2YhxM+Tqhej
x5wRQedmVINdd8vvETLoO8pUwMmM9vogNVNDi1exQc0OlX843yGFMWTiTMHSXwVU
v94Tao0nvmok5qRhBvnSgmr1/H/7VpD/rwCUDaTX9XR+6ERkRrbWdnE1AoGBAK4C
RqnOquGq0ar0vEo4ukEIZTdIvK01NwH39nbqNatFh4D82wL75gVsWrAQR1EgZWlt
elqrJ15a/E2KRQm4wWC96WlzGCEGE7c7uxc+SSAH8S0j3sXha/x01HNuMiH/OdLk
VevmmGJmx0tOkYlzgk5GGdip/iu1t8g2u17h2PYBAoGBAJVLQCi+Kz17OVRnrj0i
u0AnXiZ+4JLhbUiw505oCJB+XqsulwZopwjq5tWC9RhGBg0pf1kPVxjk4DnlkMil
kV108ayulX3WmQImWMaObkgXCjZ2E8XN4oPye4CeQi9sN52div+e1Jq1/GHfIOg2
E0Lok+SwQ+nuukOoF+8OH2Ug
-----END PRIVATE KEY-----
";

    const TEST_JWK_N: &str = "utxFl50_8KA0_RzXj7gIAqOHrIq8aYYAW-0l6ltRomMYYNDs414qZ3mbmwTSTwbvG36Fdpzg9oC2HVRvlDF8BTEPthXv3KCPjBF6WOjAzoHhRkCZRZSrLoNtVajFShLl72RRmKTECzEWQowhy-x25y0WeIS48fGKCusYtDE8L03cfoY_QA5yxiD8scY0hFcMCeb5CC9hjUppMmvKsHUVl9wBy7sz9Q22prRfV-VyXFMOP8WC6J7A79e-qi4jUcup3sOzpyPyBGu1VJoAHI040BFykYTkelCQSIMP8iOBIXhceJ0KRAGqRcV0vdumeACtDGVQGtqdYp_hnL5tdE5dRQ";
    const TEST_JWK_E: &str = "AQAB";
    const TEST_KID: &str = "test-kid-1";

    fn test_oidc_config(issuer: &str) -> OidcConfig {
        OidcConfig {
            issuer: issuer.to_string(),
            audience: "test-client-id".to_string(),
            jwks_uri: None,
            client_id: Some("test-client-id".to_string()),
            display_name: Some("Test IdP".to_string()),
            scopes: vec!["openid".into()],
            role_claims: vec!["groups".into()],
            role_map: OidcRoleMap::default(),
            deny_unmapped: false,
            redirect_uri: None,
            client_secret_file: None,
            client_secret_env: None,
        }
    }

    fn discovery_body(issuer: &str, jwks_url: &str) -> serde_json::Value {
        json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/auth"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": jwks_url,
            "userinfo_endpoint": format!("{issuer}/userinfo"),
            "code_challenge_methods_supported": ["S256"],
            "response_types_supported": ["code"],
        })
    }

    fn jwks_body() -> serde_json::Value {
        json!({
            "keys": [
                {
                    "kid": TEST_KID,
                    "kty": "RSA",
                    "use": "sig",
                    "alg": "RS256",
                    "n": TEST_JWK_N,
                    "e": TEST_JWK_E,
                }
            ]
        })
    }

    fn mint_id_token(
        iss: &str,
        aud: &str,
        exp: i64,
        nbf: Option<i64>,
        kid: Option<&str>,
    ) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = kid.map(|s| s.to_string());
        let mut claims = json!({
            "iss": iss,
            "aud": aud,
            "sub": "alice@example.com",
            "exp": exp,
            "iat": exp - 60,
            "groups": ["nexus-operators"],
        });
        if let Some(nbf) = nbf {
            claims["nbf"] = nbf.into();
        }
        let key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes())
            .expect("test RSA private key parses");
        jsonwebtoken::encode(&header, &claims, &key).expect("sign")
    }

    async fn boot_fake_idp() -> (MockServer, String) {
        let server = MockServer::start().await;
        let issuer = server.uri();
        let jwks_url = format!("{issuer}/jwks");
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(discovery_body(&issuer, &jwks_url)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body()))
            .mount(&server)
            .await;
        (server, issuer)
    }

    #[tokio::test]
    async fn discovery_and_verify_happy_path() {
        let (_server, issuer) = boot_fake_idp().await;
        let cfg = test_oidc_config(&issuer);
        let client = OidcClient::discover(cfg)
            .await
            .expect("discovery + jwks succeed");

        let now = Utc.timestamp_opt(1_900_000_000, 0).unwrap();
        let token = mint_id_token(
            &issuer,
            "test-client-id",
            now.timestamp() + 300,
            None,
            Some(TEST_KID),
        );
        let verified = client
            .verify_id_token(&token, now)
            .expect("good token verifies");
        assert_eq!(verified.claims.sub, "alice@example.com");
        assert_eq!(verified.claims.iss, issuer);
        // The flattened `extra` should carry the `groups` claim
        // so Step 3.2 can map it. Confirm the round-trip.
        let groups = verified
            .claims
            .extra
            .get("groups")
            .and_then(|v| v.as_array())
            .expect("groups present");
        assert_eq!(groups[0].as_str(), Some("nexus-operators"));
    }

    #[tokio::test]
    async fn expired_token_rejected() {
        let (_server, issuer) = boot_fake_idp().await;
        let client = OidcClient::discover(test_oidc_config(&issuer))
            .await
            .unwrap();
        let now = Utc.timestamp_opt(1_900_000_000, 0).unwrap();
        let token = mint_id_token(
            &issuer,
            "test-client-id",
            now.timestamp() - 1,
            None,
            Some(TEST_KID),
        );
        match client.verify_id_token(&token, now) {
            Err(OidcError::InvalidIdToken("expired")) => (),
            other => panic!("expected expired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn future_nbf_rejected() {
        let (_server, issuer) = boot_fake_idp().await;
        let client = OidcClient::discover(test_oidc_config(&issuer))
            .await
            .unwrap();
        let now = Utc.timestamp_opt(1_900_000_000, 0).unwrap();
        let token = mint_id_token(
            &issuer,
            "test-client-id",
            now.timestamp() + 300,
            Some(now.timestamp() + 60),
            Some(TEST_KID),
        );
        match client.verify_id_token(&token, now) {
            Err(OidcError::InvalidIdToken("not_yet_valid")) => (),
            other => panic!("expected not_yet_valid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrong_audience_rejected() {
        let (_server, issuer) = boot_fake_idp().await;
        let client = OidcClient::discover(test_oidc_config(&issuer))
            .await
            .unwrap();
        let now = Utc.timestamp_opt(1_900_000_000, 0).unwrap();
        let token = mint_id_token(
            &issuer,
            "some-other-client",
            now.timestamp() + 300,
            None,
            Some(TEST_KID),
        );
        match client.verify_id_token(&token, now) {
            Err(OidcError::InvalidIdToken("bad_aud")) => (),
            other => panic!("expected bad_aud, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wrong_issuer_rejected() {
        let (_server, issuer) = boot_fake_idp().await;
        let client = OidcClient::discover(test_oidc_config(&issuer))
            .await
            .unwrap();
        let now = Utc.timestamp_opt(1_900_000_000, 0).unwrap();
        let token = mint_id_token(
            "https://evil.example.com",
            "test-client-id",
            now.timestamp() + 300,
            None,
            Some(TEST_KID),
        );
        match client.verify_id_token(&token, now) {
            Err(OidcError::InvalidIdToken("bad_iss")) => (),
            other => panic!("expected bad_iss, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_kid_rejected() {
        let (_server, issuer) = boot_fake_idp().await;
        let client = OidcClient::discover(test_oidc_config(&issuer))
            .await
            .unwrap();
        let now = Utc.timestamp_opt(1_900_000_000, 0).unwrap();
        let token = mint_id_token(
            &issuer,
            "test-client-id",
            now.timestamp() + 300,
            None,
            Some("not-the-real-kid"),
        );
        match client.verify_id_token(&token, now) {
            Err(OidcError::InvalidIdToken("unknown_kid")) => (),
            other => panic!("expected unknown_kid, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn discovery_issuer_mismatch_fails_construction() {
        let server = MockServer::start().await;
        let configured = format!("{}/expected", server.uri());
        let jwks_url = format!("{}/jwks", server.uri());
        // Discovery doc lies about its `issuer`.
        Mock::given(method("GET"))
            .and(path("/expected/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": "https://something.else/",
                "authorization_endpoint": format!("{configured}/auth"),
                "token_endpoint": format!("{configured}/token"),
                "jwks_uri": jwks_url,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body()))
            .mount(&server)
            .await;

        let cfg = test_oidc_config(&configured);
        match OidcClient::discover(cfg).await {
            Err(OidcError::InvalidDiscovery(msg)) => {
                assert!(
                    msg.contains("issuer mismatch"),
                    "expected issuer-mismatch tag, got {msg:?}"
                );
            }
            other => panic!("expected InvalidDiscovery, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn jwks_rotation_refresh_swaps_keys() {
        // Validates the refresh() path: a JWKS rotation at the
        // IdP (e.g. a kid swap) should be picked up by a manual
        // refresh without reconstructing the client. We don't
        // wait for the 1h background interval — that's an
        // implementation detail of `spawn_refresh`.
        let server = MockServer::start().await;
        let issuer = server.uri();
        let jwks_url = format!("{issuer}/jwks");

        // First request: serve the initial JWKS with TEST_KID.
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(discovery_body(&issuer, &jwks_url)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jwks_body()))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let client = OidcClient::discover(test_oidc_config(&issuer))
            .await
            .unwrap();

        let now = Utc.timestamp_opt(1_900_000_000, 0).unwrap();
        let original = mint_id_token(
            &issuer,
            "test-client-id",
            now.timestamp() + 300,
            None,
            Some(TEST_KID),
        );
        assert!(client.verify_id_token(&original, now).is_ok());

        // Rotate: next JWKS fetch returns the SAME key under a
        // different kid. The old kid is now gone — a token
        // signed under it should fail with unknown_kid after
        // the refresh.
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "keys": [{
                    "kid": "rotated-kid",
                    "kty": "RSA",
                    "use": "sig",
                    "alg": "RS256",
                    "n": TEST_JWK_N,
                    "e": TEST_JWK_E,
                }]
            })))
            .mount(&server)
            .await;

        client.refresh().await.expect("refresh");

        match client.verify_id_token(&original, now) {
            Err(OidcError::InvalidIdToken("unknown_kid")) => (),
            other => panic!("expected unknown_kid post-rotation, got {other:?}"),
        }

        let rotated = mint_id_token(
            &issuer,
            "test-client-id",
            now.timestamp() + 300,
            None,
            Some("rotated-kid"),
        );
        assert!(client.verify_id_token(&rotated, now).is_ok());
    }

    #[test]
    fn discovery_url_strips_trailing_slash() {
        assert_eq!(
            discovery_url("https://idp.example.com/"),
            "https://idp.example.com/.well-known/openid-configuration"
        );
        assert_eq!(
            discovery_url("https://idp.example.com"),
            "https://idp.example.com/.well-known/openid-configuration"
        );
    }

    #[test]
    fn oct_kty_is_rejected_at_parse_time() {
        // A pure-`oct` JWKS yields NoUsableKeys — shared-secret
        // OIDC is treated as misconfiguration.
        let doc = JwksDocument {
            keys: vec![Jwk {
                kid: Some("k".into()),
                kty: "oct".into(),
                r#use: Some("sig".into()),
                alg: Some("HS256".into()),
                n: None,
                e: None,
                crv: None,
                x: None,
                y: None,
            }],
        };
        match prepare_jwks(doc) {
            Err(OidcError::NoUsableKeys) => (),
            other => panic!("expected NoUsableKeys, got {other:?}"),
        }
    }

    #[test]
    fn sole_key_fallback_when_no_kid() {
        // Some single-key IdPs publish a JWKS with one entry
        // and no `kid`. The validator must still accept tokens
        // that also have no `kid`.
        let doc = JwksDocument {
            keys: vec![Jwk {
                kid: None,
                kty: "RSA".into(),
                r#use: Some("sig".into()),
                alg: Some("RS256".into()),
                n: Some(TEST_JWK_N.to_string()),
                e: Some(TEST_JWK_E.to_string()),
                crv: None,
                x: None,
                y: None,
            }],
        };
        let prepared = prepare_jwks(doc).expect("sole key prepares");
        assert!(prepared.by_kid.is_empty());
        assert!(prepared.sole_key.is_some());
    }
}
