//! OAuth refresh-token encryption at rest (M2.2 Phase 2).
//!
//! ## Why this module exists
//!
//! Phase 2 lets operators connect a Google Drive or OneDrive cold
//! backend. Both providers issue **long-lived refresh tokens** that
//! the engine needs to keep on disk so the cold replicator can mint
//! short-lived access tokens forever after the operator finishes
//! the OAuth dance.
//!
//! A refresh token is a bearer credential — anyone who reads
//! `nexus.db` with the file in plaintext gets full `drive.file` /
//! `Files.ReadWrite.AppFolder` access to the operator's cloud
//! account. We MUST encrypt them at rest so a `motion_clips.db`
//! backup, a casual SD-card grab, or an opportunistic
//! `cat storage_backends.config_json` cannot leak the credential.
//!
//! ## The contract
//!
//! * **Key:** AES-256 (32 bytes) derived via HKDF-SHA256 from the
//!   admin-auth secret (the same `admin-auth.json:secret` the
//!   HS256 JWT bearer signs with). Salt
//!   `"nexus-cold-tier.v1"`, info `"oauth-refresh-token"`, L=32 —
//!   matches plan-of-record §157 verbatim.
//! * **Cipher:** AES-256-GCM (96-bit nonce, 128-bit auth tag). A
//!   **fresh random nonce per encryption** is non-negotiable —
//!   nonce reuse with the same key is a catastrophic AES-GCM
//!   failure (key recovery via XOR of the two ciphertexts).
//! * **Wire format:** [`EncryptedToken`] serde struct, stored as a
//!   JSON blob inside `storage_backends.config_json`:
//!   `{"alg":"AES-256-GCM","nonce":"<b64>","ct":"<b64>"}`. The
//!   `alg` field is a forward-compatibility marker — when we
//!   rotate to v2 (e.g. switching to ChaCha20-Poly1305) the
//!   decryptor can dispatch on it.
//! * **Logging hygiene:** [`RefreshToken`] has a redacted `Debug`
//!   impl so accidental `tracing::info!(token = ?t, ...)` calls
//!   render `"<redacted>"` instead of leaking the plaintext into
//!   logs that ship to operators or backups. Drops zeroize the
//!   underlying bytes.
//!
//! ## What this module deliberately does NOT do
//!
//! * **No key rotation in this PR.** The HKDF salt is versioned
//!   (`nexus-cold-tier.v1`) so a future migration can re-encrypt
//!   under `v2` and we'll know which key to try first; the
//!   re-encryption tool itself is out of scope.
//! * **No hardware-backed keys (TPM/Secure Enclave).** The threat
//!   model is "filesystem-level disclosure" not "physical device
//!   compromise". A hardware key would be a Phase-N follow-up.
//! * **No envelope encryption.** A single derived key wraps every
//!   refresh token. Refresh tokens are the only secret material
//!   that needs this treatment today; if/when we add per-camera
//!   recording keys the envelope wrapper is the right place to
//!   start.

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// HKDF-SHA256 salt. Versioned so we can rotate the KDF in a future
/// migration without losing the ability to decrypt v1 ciphertexts
/// (the wire-format `alg` field carries the matching version
/// marker).
const HKDF_SALT: &[u8] = b"nexus-cold-tier.v1";

/// HKDF-SHA256 info string. Scoped to refresh-token encryption so a
/// future use of the same admin secret for a different purpose
/// (e.g. session-cookie signing) derives a different key.
const HKDF_INFO: &[u8] = b"oauth-refresh-token";

/// AEAD algorithm marker written into [`EncryptedToken::alg`]. The
/// decryptor refuses anything else so a v2 rotation can dispatch
/// cleanly. Don't change this without bumping the salt version.
pub const ALG_AES_256_GCM_V1: &str = "AES-256-GCM";

/// Errors the encrypt / decrypt path can surface. All variants
/// scrub their input on the way out — the `Debug` impl on this
/// type does NOT contain ciphertext bytes, key material, or any
/// part of the plaintext.
#[derive(Debug, Error)]
pub enum TokenCryptoError {
    /// HKDF expansion failed. Effectively impossible at L=32 but
    /// the API surface returns `Result` so we propagate cleanly.
    #[error("key derivation failed")]
    KeyDerivation,

    /// AES-GCM AEAD failure on encrypt or decrypt. Covers
    /// wrong-key, tampered ciphertext, tampered nonce, and
    /// truncated input — we deliberately collapse them into one
    /// error because distinguishing them client-side leaks
    /// information about which part of the blob is corrupt.
    #[error("AEAD authentication failed")]
    Aead,

    /// The `alg` field on the wire is not one this build
    /// recognises. Reading a v2 blob with a v1 binary lands here.
    #[error("unsupported algorithm marker: {0:?}")]
    UnsupportedAlg(String),

    /// `nonce` or `ct` is not valid base64. Includes corrupt
    /// `config_json` and hand-edited entries.
    #[error("malformed base64 in encrypted token")]
    Base64,

    /// Nonce length is not 96 bits / 12 bytes. Always rejected;
    /// AES-GCM nonces are exactly that size.
    #[error("invalid nonce length: expected 12, got {0}")]
    NonceLength(usize),
}

/// On-disk representation of an encrypted refresh token. Lives
/// inside `storage_backends.config_json` as a self-describing JSON
/// blob.
///
/// Stable wire shape — changes here require a `config_json`
/// migration. Add new variants by introducing a new `alg` marker
/// rather than reshaping this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedToken {
    /// Algorithm identifier. Must equal [`ALG_AES_256_GCM_V1`] for
    /// this build's decryptor to accept it.
    pub alg: String,
    /// Base64-encoded 96-bit nonce. Random per encryption — nonce
    /// reuse with the same key is a catastrophic AES-GCM failure.
    pub nonce: String,
    /// Base64-encoded ciphertext + 128-bit AEAD auth tag (appended
    /// by aes-gcm's `Aead::encrypt`).
    pub ct: String,
}

/// In-memory plaintext refresh token. Wraps a `String` so callers
/// have to go through the encrypt path explicitly to persist it,
/// and so `Debug` can redact the value before it lands in a
/// tracing event.
///
/// Implements [`ZeroizeOnDrop`] so the underlying bytes are
/// scrubbed when the value leaves scope; this is best-effort
/// (compiler optimisations + heap reallocations can leave
/// fragments) but raises the bar above "Rust just leaks it on the
/// heap forever".
#[derive(Clone, ZeroizeOnDrop)]
pub struct RefreshToken(String);

impl RefreshToken {
    /// Construct from a freshly-minted OAuth response. The string
    /// is moved in and never duplicated; if the caller still holds
    /// a `String` view of the same bytes, the duplicate is on the
    /// caller — we can't reach across the ABI to zeroize it.
    pub fn new(s: String) -> Self {
        Self(s)
    }

    /// Plaintext access for the OAuth refresh HTTP call. Use
    /// sparingly — every callsite is a potential leak surface.
    /// Returns `&str` (not `String`) so we don't multiply the
    /// number of copies in flight.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Length of the underlying token. Useful for sanity-checking
    /// "did the OAuth endpoint actually return something" without
    /// having to expose the secret in a log line.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True iff the underlying token string is empty. Mostly here
    /// because clippy nags about `len()` without `is_empty()`.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER leak the plaintext into logs / panic messages.
        // Surface the length only — useful for "did we get a
        // sensible-looking token" diagnostics without divulging
        // the credential.
        write!(f, "RefreshToken(<redacted, len={}>)", self.0.len())
    }
}

/// Derive the 32-byte AES-256 key from the admin-auth secret. Pure
/// function — same input always produces the same key, which is
/// the whole point (operators don't want a key rotation every time
/// they restart).
///
/// `secret` is the operator-configured admin secret (typically the
/// `admin-auth.json:secret` field). It is treated as opaque bytes;
/// any UTF-8 string survives the trip through HKDF intact.
fn derive_key(secret: &str) -> Result<[u8; 32], TokenCryptoError> {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), secret.as_bytes());
    let mut okm = [0u8; 32];
    hk.expand(HKDF_INFO, &mut okm)
        .map_err(|_| TokenCryptoError::KeyDerivation)?;
    Ok(okm)
}

/// Encrypt a plaintext refresh token. Generates a fresh random
/// 96-bit nonce from the OS RNG and writes both nonce + ciphertext
/// (with appended AEAD auth tag) into the returned
/// 32 fresh OS-random bytes, suitable for OAuth CSRF state or any
/// other unguessable token. Exposed here so peer crates that don't
/// otherwise depend on `aes-gcm` (notably `nexus-storage-cloud` and
/// `nexus-engine`) can share the same RNG source instead of each
/// pulling its own `rand` / `getrandom`. Not for key material —
/// derive those via [`encrypt`] / HKDF.
pub fn random_state_bytes() -> [u8; 32] {
    use aes_gcm::aead::rand_core::RngCore;
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

/// [`EncryptedToken`] in base64 form.
///
/// Two calls with the same `(secret, plaintext)` produce
/// **different** [`EncryptedToken`] values because the nonce is
/// fresh — this is intentional and an AES-GCM correctness
/// invariant. The test `encrypt_uses_fresh_nonce_each_call` covers
/// it explicitly.
pub fn encrypt(secret: &str, plaintext: &str) -> Result<EncryptedToken, TokenCryptoError> {
    let mut key_bytes = derive_key(secret)?;
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);

    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| TokenCryptoError::Aead)?;

    // Best-effort scrub of the derived key material. The cipher's
    // internal copy is its own problem (it's `Zeroize` in the
    // current aes-gcm release but we don't rely on that).
    key_bytes.zeroize();

    Ok(EncryptedToken {
        alg: ALG_AES_256_GCM_V1.to_string(),
        nonce: B64.encode(nonce.as_slice()),
        ct: B64.encode(ct),
    })
}

/// Decrypt an [`EncryptedToken`] back into a [`RefreshToken`].
/// Errors collapse the "wrong key", "tampered ciphertext", and
/// "tampered nonce" cases into a single [`TokenCryptoError::Aead`]
/// variant — distinguishing them would leak structural information
/// about the blob to an attacker who can observe the error.
pub fn decrypt(secret: &str, encrypted: &EncryptedToken) -> Result<RefreshToken, TokenCryptoError> {
    if encrypted.alg != ALG_AES_256_GCM_V1 {
        return Err(TokenCryptoError::UnsupportedAlg(encrypted.alg.clone()));
    }

    let nonce_bytes = B64
        .decode(encrypted.nonce.as_bytes())
        .map_err(|_| TokenCryptoError::Base64)?;
    if nonce_bytes.len() != 12 {
        return Err(TokenCryptoError::NonceLength(nonce_bytes.len()));
    }
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ct = B64
        .decode(encrypted.ct.as_bytes())
        .map_err(|_| TokenCryptoError::Base64)?;

    let mut key_bytes = derive_key(secret)?;
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
    let cipher = Aes256Gcm::new(key);

    let pt = cipher
        .decrypt(nonce, ct.as_slice())
        .map_err(|_| TokenCryptoError::Aead)?;

    key_bytes.zeroize();

    // The plaintext is UTF-8 by construction (we encrypted from a
    // `&str` in `encrypt`). If it's not, the blob was forged with
    // a key we happen to know — treat as AEAD failure rather than
    // silently surfacing arbitrary bytes.
    let s = String::from_utf8(pt).map_err(|_| TokenCryptoError::Aead)?;
    Ok(RefreshToken::new(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SECRET: &str = "correct horse battery staple";
    const TEST_TOKEN: &str = "1//0ehGy0Hg-WaZsCgYIARAAGA4SNgF-L9IrlWAB2cF6pE_example_refresh_token";

    #[test]
    fn round_trip_recovers_exact_plaintext() {
        let enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        let dec = decrypt(TEST_SECRET, &enc).unwrap();
        assert_eq!(dec.expose_secret(), TEST_TOKEN);
    }

    #[test]
    fn alg_marker_is_stable_v1() {
        let enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        assert_eq!(enc.alg, ALG_AES_256_GCM_V1);
        assert_eq!(enc.alg, "AES-256-GCM", "wire-format breakage");
    }

    #[test]
    fn encrypt_uses_fresh_nonce_each_call() {
        // AES-GCM requires nonces never repeat under the same key.
        // The encryption path uses `OsRng`-derived nonces; two
        // encryptions of the same plaintext must yield different
        // (nonce, ct) pairs. Probabilistic but the collision space
        // is 2^96 — failing this test is a real bug, not a flake.
        let a = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        let b = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        assert_ne!(a.nonce, b.nonce, "nonce reuse — catastrophic for AES-GCM");
        assert_ne!(a.ct, b.ct, "ciphertext must differ when nonce does");
        // But both decrypt to the same plaintext.
        assert_eq!(
            decrypt(TEST_SECRET, &a).unwrap().expose_secret(),
            TEST_TOKEN
        );
        assert_eq!(
            decrypt(TEST_SECRET, &b).unwrap().expose_secret(),
            TEST_TOKEN
        );
    }

    #[test]
    fn wrong_secret_fails_authentication() {
        let enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        let err = decrypt("wrong secret", &enc).unwrap_err();
        assert!(matches!(err, TokenCryptoError::Aead));
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let mut enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        // Flip a bit in the middle of the ciphertext — must NOT
        // round-trip. AEAD auth tag should catch this.
        let mut raw = B64.decode(enc.ct.as_bytes()).unwrap();
        let mid = raw.len() / 2;
        raw[mid] ^= 0x01;
        enc.ct = B64.encode(&raw);
        let err = decrypt(TEST_SECRET, &enc).unwrap_err();
        assert!(matches!(err, TokenCryptoError::Aead));
    }

    #[test]
    fn tampered_nonce_fails_authentication() {
        let mut enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        let mut nonce_bytes = B64.decode(enc.nonce.as_bytes()).unwrap();
        nonce_bytes[0] ^= 0xff;
        enc.nonce = B64.encode(&nonce_bytes);
        let err = decrypt(TEST_SECRET, &enc).unwrap_err();
        assert!(matches!(err, TokenCryptoError::Aead));
    }

    #[test]
    fn unknown_alg_marker_rejected() {
        let mut enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        enc.alg = "AES-256-GCM-v2".to_string();
        let err = decrypt(TEST_SECRET, &enc).unwrap_err();
        match err {
            TokenCryptoError::UnsupportedAlg(s) => assert_eq!(s, "AES-256-GCM-v2"),
            other => panic!("expected UnsupportedAlg, got {other:?}"),
        }
    }

    #[test]
    fn malformed_base64_rejected_cleanly() {
        let mut enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        enc.ct = "@@@not-base64@@@".to_string();
        let err = decrypt(TEST_SECRET, &enc).unwrap_err();
        assert!(matches!(err, TokenCryptoError::Base64));
    }

    #[test]
    fn wrong_nonce_length_rejected() {
        let mut enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        // 8-byte nonce — AES-GCM requires exactly 12.
        enc.nonce = B64.encode([0u8; 8]);
        let err = decrypt(TEST_SECRET, &enc).unwrap_err();
        assert!(matches!(err, TokenCryptoError::NonceLength(8)));
    }

    #[test]
    fn wire_format_is_self_describing_json() {
        // The blob must round-trip through serde_json::Value so
        // it can live inside `storage_backends.config_json` as a
        // sub-object without bespoke parsing.
        let enc = encrypt(TEST_SECRET, TEST_TOKEN).unwrap();
        let json = serde_json::to_string(&enc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["alg"], "AES-256-GCM");
        assert!(parsed["nonce"].is_string());
        assert!(parsed["ct"].is_string());
        // And round-trips back into an EncryptedToken.
        let round: EncryptedToken = serde_json::from_value(parsed).unwrap();
        assert_eq!(round, enc);
        assert_eq!(
            decrypt(TEST_SECRET, &round).unwrap().expose_secret(),
            TEST_TOKEN
        );
    }

    #[test]
    fn refresh_token_debug_is_redacted() {
        let t = RefreshToken::new(TEST_TOKEN.to_string());
        let s = format!("{t:?}");
        assert!(
            !s.contains(TEST_TOKEN),
            "RefreshToken Debug leaked plaintext: {s}"
        );
        assert!(s.contains("redacted"));
        assert!(s.contains(&format!("len={}", TEST_TOKEN.len())));
    }

    #[test]
    fn empty_plaintext_is_legal() {
        // OAuth endpoints can theoretically return empty strings
        // on bad inputs; we shouldn't blow up on encrypt and we
        // should round-trip cleanly. (The caller is expected to
        // reject empty tokens before they reach disk; this test
        // just locks the cipher boundary.)
        let enc = encrypt(TEST_SECRET, "").unwrap();
        let dec = decrypt(TEST_SECRET, &enc).unwrap();
        assert!(dec.is_empty());
        assert_eq!(dec.len(), 0);
        assert_eq!(dec.expose_secret(), "");
    }

    #[test]
    fn derived_key_is_deterministic_for_same_secret() {
        let k1 = derive_key(TEST_SECRET).unwrap();
        let k2 = derive_key(TEST_SECRET).unwrap();
        assert_eq!(k1, k2, "HKDF must be deterministic for the same input");
        // And different secrets yield different keys.
        let k3 = derive_key("different").unwrap();
        assert_ne!(k1, k3);
    }
}
