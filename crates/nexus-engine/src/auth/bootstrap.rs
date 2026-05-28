//! First-boot bootstrap helpers.
//!
//! Historical context: M6 Phase 2 Step 2.6 used to mint a
//! random one-time admin password at engine startup and write
//! it to `<state_dir>/bootstrap-password.txt` for the
//! installer's closing banner. That flow was retired in favor
//! of a UI-first design: the engine boots with no admin user,
//! and the operator picks the initial admin password via
//! `POST /v1/auth/first-run-setup` (see
//! [`crate::auth::login::post_first_run_setup`]). See
//! `docs/M_ADMIN.md` for the rationale.
//!
//! What's left in this module:
//!
//! * [`BOOTSTRAP_USERNAME`] — the username the first-run-setup
//!   handler defaults to when the operator submits a blank
//!   username field. Also the username allocated by the
//!   `set-admin-password` CLI recovery path.
//! * [`generate_one_time_password`] — entropy + encoding for
//!   any admin-reset-password CLI / API surface that needs to
//!   surface a fresh random password to the operator. Used by
//!   [`crate::auth::users_admin`].
//! * [`bootstrap_sentinel_path`] / [`clear_bootstrap_sentinel`]
//!   — best-effort cleanup of any stale OTP file left behind
//!   by an older install. Called by the first-run-setup
//!   handler, the change-password handler, and engine startup.

use std::io;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// Filename of the bootstrap-password sentinel inside the
/// engine's `state_dir`. Older installs used to write the
/// generated one-time admin password here; current installs
/// never produce this file but we still clean it up on
/// startup and on every successful change-password so a
/// machine upgraded from an older release doesn't carry
/// the stale OTP forward.
pub const BOOTSTRAP_SENTINEL_FILE: &str = "bootstrap-password.txt";

/// Default username for the first admin account. The UI's
/// first-run-setup form prefills the username field with
/// this literal; the [`crate::auth::login::post_first_run_setup`]
/// handler falls back to this if the operator submits a blank
/// username; the `set-admin-password` CLI also uses this when
/// no `--username` is supplied. The operator can rename /
/// disable / soft-delete this user after first login.
pub const BOOTSTRAP_USERNAME: &str = "admin";

/// Entropy budget for an admin-reset one-time password.
/// 24 random bytes → 32-char URL-safe-base64-no-pad string.
/// 192 bits is excessive for a string the operator will rotate
/// out within minutes, but the cost is one `getrandom` call.
pub const BOOTSTRAP_PASSWORD_BYTES: usize = 24;

/// Generate a random one-time password suitable for surfacing
/// to an operator (e.g. an admin-CRUD "reset password" path).
/// Caller is responsible for hashing the result before
/// persistence and for never logging it more than once.
pub fn generate_one_time_password() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; BOOTSTRAP_PASSWORD_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Resolve the on-disk path of the bootstrap-password sentinel
/// relative to the engine's `state_dir`. Pure path arithmetic —
/// does not touch the filesystem.
pub fn bootstrap_sentinel_path(state_dir: &Path) -> PathBuf {
    state_dir.join(BOOTSTRAP_SENTINEL_FILE)
}

/// Best-effort delete the bootstrap-password sentinel file.
/// Called on engine startup (to clean up any file from an
/// older release that used to mint OTPs), by the first-run-setup
/// handler (to clear any pre-existing OTP the moment the
/// operator picks a real password), and by the change-password
/// handler (same reason, for the case where a UI-set password
/// is rotated).
///
/// Never fails the caller: a missing file is success; any
/// other I/O failure is logged at `warn!` and swallowed.
pub fn clear_bootstrap_sentinel(state_dir: &Path) {
    let path = bootstrap_sentinel_path(state_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            tracing::info!(path = %path.display(), "bootstrap-password sentinel removed");
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Already cleared (or never existed on this install).
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "failed to remove bootstrap-password sentinel; remove it manually",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ----- generate_one_time_password -------------------------------------

    #[test]
    fn one_time_password_is_32_chars() {
        // 24 bytes URL-safe-base64-no-pad = ceil(24 * 4 / 3) = 32.
        let pw = generate_one_time_password().unwrap();
        assert_eq!(pw.len(), 32, "{pw}");
    }

    #[test]
    fn one_time_password_is_url_safe_base64_no_pad() {
        let pw = generate_one_time_password().unwrap();
        for c in pw.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "non url-safe-base64 char {c:?} in {pw}"
            );
        }
        assert!(!pw.contains('='), "padding leaked: {pw}");
    }

    #[test]
    fn two_one_time_passwords_differ() {
        // Cheap entropy smoke — 192 bits, collision probability
        // is ~0 in any finite test budget.
        let a = generate_one_time_password().unwrap();
        let b = generate_one_time_password().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn generated_password_satisfies_policy() {
        // The policy gate inside hash_password() rejects
        // too-short / common passwords. A regression that
        // tightens MIN_PASSWORD_LEN past 32 would break the
        // admin-reset path silently if we didn't pin this.
        let pw = generate_one_time_password().unwrap();
        assert!(super::super::passwords::hash_password(&pw).is_ok(), "{pw}");
    }

    // ----- sentinel helpers ----------------------------------------------

    #[test]
    fn bootstrap_sentinel_path_is_under_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = bootstrap_sentinel_path(dir.path());
        assert_eq!(p, dir.path().join(BOOTSTRAP_SENTINEL_FILE));
    }

    #[test]
    fn clear_bootstrap_sentinel_is_idempotent_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        // No file exists yet — must not panic.
        clear_bootstrap_sentinel(dir.path());
        clear_bootstrap_sentinel(dir.path());
    }

    #[test]
    fn clear_bootstrap_sentinel_removes_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = bootstrap_sentinel_path(dir.path());
        std::fs::write(&path, "stale\tone-time-password\n").unwrap();
        assert!(path.exists());
        clear_bootstrap_sentinel(dir.path());
        assert!(!path.exists());
    }
}
