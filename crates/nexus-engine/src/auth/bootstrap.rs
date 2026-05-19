//! M6 Phase 2 Step 2.6 — first-boot bootstrap.
//!
//! When `auth.mode` permits local users (Local or Hybrid) AND
//! the `users` table is empty (incl. tombstones), provision a
//! single `admin` user with a generated one-time password and
//! `force_password_reset = true`. The plaintext password is
//! returned to the caller exactly once so engine `main` can
//! surface it via `tracing::warn!`; the engine MUST NOT persist
//! it anywhere else.
//!
//! Counting tombstones in the "is empty?" check is deliberate —
//! see [`Store::count_users`] for the security rationale.
//!
//! Cli recovery for "operator soft-deleted the only admin"
//! lives outside this module (Step 2.8 ships the admin-CRUD
//! that includes restore; pre-2.8 the operator must edit SQLite
//! directly). We do not auto-re-bootstrap.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use nexus_config::AuthMode;
use nexus_store::{NewUser, Store, StoreError, UserId, UsersError};
use nexus_types::Role;

use super::passwords::{hash_password, PasswordError};

/// Username allocated to the bootstrap admin. The operator can
/// rename / disable / soft-delete this user after first login,
/// but the bootstrap path always uses this literal so the
/// operator knows what to type into the login form on day one.
pub const BOOTSTRAP_USERNAME: &str = "admin";

/// Entropy budget for the one-time password. 24 random bytes →
/// 32-char URL-safe-base64-no-pad string. 192 bits of entropy
/// is excessive for a string the operator is meant to rotate
/// out within minutes of seeing it, but the cost is zero
/// (one `getrandom` call) and it removes any "is this strong
/// enough?" question from review.
pub const BOOTSTRAP_PASSWORD_BYTES: usize = 24;

/// Outcome of [`bootstrap_if_needed`]. The engine binary's
/// `main` matches on this to decide whether to emit the
/// one-time-password WARN line. Existing-bootstrap and
/// disallowed-mode paths are NOT errors — they're the steady
/// state for any non-first boot.
#[derive(Debug)]
pub enum BootstrapOutcome {
    /// `auth.mode` doesn't allow local users (e.g. `Oidc`,
    /// `DevToken`, `None`). Nothing to do.
    SkippedModeDisallowsLocal,
    /// `users` table already has at least one row (active,
    /// disabled, or tombstoned). Nothing to do.
    SkippedAlreadyBootstrapped,
    /// Created the initial admin. The plaintext one-time
    /// password is returned here exactly once; the caller
    /// must log it and drop the value.
    AdminCreated {
        user_id: UserId,
        username: String,
        one_time_password: String,
    },
}

/// All the ways bootstrap can break. None of these are recoverable
/// without operator action; engine `main` should treat any `Err`
/// here as a fatal startup failure.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// `getrandom` couldn't produce password bytes. On a healthy
    /// OS this is impossible; surfaces as "no entropy source"
    /// inside a hermetic test sandbox.
    #[error("rng: {0}")]
    Rng(#[from] getrandom::Error),
    /// argon2id hashing or the password-policy gate rejected
    /// the generated password. Defence-in-depth: the generated
    /// 32-char URL-safe-base64 password is well over the
    /// [`super::passwords::MIN_PASSWORD_LEN`] floor and won't
    /// collide with the common-password denylist, but a future
    /// tightening of the policy could trip this.
    #[error("password: {0}")]
    Password(#[from] PasswordError),
    /// Insert into `users` failed for any reason other than a
    /// raw SQL error. In practice the only realistic variant
    /// is `UsernameTaken` if a parallel boot raced us — we
    /// don't try to recover because the very next call to
    /// [`bootstrap_if_needed`] will return
    /// [`BootstrapOutcome::SkippedAlreadyBootstrapped`].
    #[error("users: {0}")]
    Users(#[from] UsersError),
    /// Raw `count_users` SQL failure. Almost certainly a
    /// disk-pressure or schema-drift indicator; not auto-
    /// recoverable.
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Run the first-boot check and conditionally create the admin
/// user. Idempotent: safe to call on every engine start —
/// bootstrap fires exactly once across the lifetime of the
/// database file.
///
/// The function is `async` solely because the underlying
/// [`Store`] methods are; the actual logic is cheap (one
/// `COUNT(*)`, one `getrandom`, one argon2id hash, one
/// `INSERT`).
pub async fn bootstrap_if_needed(
    store: &Store,
    mode: AuthMode,
) -> Result<BootstrapOutcome, BootstrapError> {
    if !mode.allows_local() {
        return Ok(BootstrapOutcome::SkippedModeDisallowsLocal);
    }
    let n = store.count_users().await?;
    if n > 0 {
        return Ok(BootstrapOutcome::SkippedAlreadyBootstrapped);
    }
    let one_time_password = generate_one_time_password()?;
    let hash = hash_password(&one_time_password)?;
    let user_id = store
        .create_user(&NewUser {
            username: BOOTSTRAP_USERNAME,
            role: Role::Admin,
            password_hash: Some(&hash),
            oidc_subject_hash: None,
            force_password_reset: true,
        })
        .await?;
    Ok(BootstrapOutcome::AdminCreated {
        user_id,
        username: BOOTSTRAP_USERNAME.to_string(),
        one_time_password,
    })
}

/// Generate the one-time password. Public so the admin-CRUD
/// "reset password" handler (Step 2.8) can reuse the exact
/// same entropy budget + encoding when an admin asks for a
/// random reset.
pub fn generate_one_time_password() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; BOOTSTRAP_PASSWORD_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_config::StoreConfig;
    use nexus_store::Store;
    use std::path::PathBuf;
    use tempfile::TempDir;

    async fn fresh_store() -> (Store, TempDir) {
        let dir = tempfile::tempdir().expect("tmpdir");
        let db_path = dir.path().join("nexus.db");
        let cfg = StoreConfig {
            url: format!("sqlite:{}?mode=rwc", db_path.display()),
            seed_from_config: false,
            duckdb_attach: false,
            duckdb_path: PathBuf::from("/tmp/unused.duckdb"),
        };
        let store = Store::open(&cfg).await.expect("Store::open");
        (store, dir)
    }

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
        // tightens MIN_PASSWORD_LEN past 32 would break
        // bootstrap silently if we didn't pin this.
        let pw = generate_one_time_password().unwrap();
        assert!(hash_password(&pw).is_ok(), "{pw}");
    }

    // ----- bootstrap_if_needed: mode gating -------------------------------

    #[tokio::test]
    async fn skips_when_mode_disallows_local() {
        let (store, _g) = fresh_store().await;
        for mode in [AuthMode::None, AuthMode::DevToken, AuthMode::Oidc] {
            let out = bootstrap_if_needed(&store, mode).await.unwrap();
            assert!(
                matches!(out, BootstrapOutcome::SkippedModeDisallowsLocal),
                "mode={mode:?} got {out:?}"
            );
        }
        // count_users still zero — we did not write anything.
        assert_eq!(store.count_users().await.unwrap(), 0);
    }

    // ----- bootstrap_if_needed: first boot --------------------------------

    #[tokio::test]
    async fn creates_admin_on_first_boot_local() {
        let (store, _g) = fresh_store().await;
        let out = bootstrap_if_needed(&store, AuthMode::Local).await.unwrap();
        match out {
            BootstrapOutcome::AdminCreated {
                user_id,
                username,
                one_time_password,
            } => {
                assert!(user_id > 0);
                assert_eq!(username, BOOTSTRAP_USERNAME);
                assert_eq!(one_time_password.len(), 32);
            }
            other => panic!("expected AdminCreated, got {other:?}"),
        }
        let row = store
            .get_user_by_username(BOOTSTRAP_USERNAME)
            .await
            .unwrap()
            .expect("admin row");
        assert_eq!(row.role, Role::Admin);
        assert!(row.force_password_reset);
        assert!(!row.disabled);
        assert!(row.deleted_at.is_none());
        assert!(row.has_password);
        assert!(!row.has_oidc);
    }

    #[tokio::test]
    async fn creates_admin_on_first_boot_hybrid() {
        // Hybrid mode is just Local + OIDC enabled. The same
        // bootstrap rules apply because hybrid needs the
        // break-glass local admin.
        let (store, _g) = fresh_store().await;
        let out = bootstrap_if_needed(&store, AuthMode::Hybrid).await.unwrap();
        assert!(matches!(out, BootstrapOutcome::AdminCreated { .. }));
    }

    // ----- bootstrap_if_needed: idempotency -------------------------------

    #[tokio::test]
    async fn second_call_is_skipped_already_bootstrapped() {
        let (store, _g) = fresh_store().await;
        let first = bootstrap_if_needed(&store, AuthMode::Local).await.unwrap();
        assert!(matches!(first, BootstrapOutcome::AdminCreated { .. }));
        let second = bootstrap_if_needed(&store, AuthMode::Local).await.unwrap();
        assert!(
            matches!(second, BootstrapOutcome::SkippedAlreadyBootstrapped),
            "{second:?}"
        );
        // And there's still only one user.
        assert_eq!(store.count_users().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn soft_deleted_admin_still_blocks_rebootstrap() {
        // Security regression guard: if the only admin was
        // soft-deleted, a fresh bootstrap would silently print
        // a one-time password to anyone tailing the log.
        let (store, _g) = fresh_store().await;
        let out = bootstrap_if_needed(&store, AuthMode::Local).await.unwrap();
        let user_id = match out {
            BootstrapOutcome::AdminCreated { user_id, .. } => user_id,
            other => panic!("expected AdminCreated, got {other:?}"),
        };
        // Soft-delete via the standard CRUD path. We bypass
        // last-admin protection by promoting a placeholder
        // admin first so `delete_user` doesn't refuse.
        let placeholder_hash = hash_password("placeholder-password-12345").unwrap();
        store
            .create_user(&NewUser {
                username: "secondary",
                role: Role::Admin,
                password_hash: Some(&placeholder_hash),
                oidc_subject_hash: None,
                force_password_reset: true,
            })
            .await
            .unwrap();
        store.soft_delete_user(user_id).await.unwrap();

        // Even though no active admin called "admin" exists,
        // count_users sees the tombstone + the placeholder,
        // so we DON'T re-bootstrap.
        let out = bootstrap_if_needed(&store, AuthMode::Local).await.unwrap();
        assert!(
            matches!(out, BootstrapOutcome::SkippedAlreadyBootstrapped),
            "{out:?}"
        );
    }

    #[tokio::test]
    async fn preexisting_non_admin_user_blocks_bootstrap() {
        // Operator manually inserted a viewer via CLI before
        // ever flipping mode to Local. Bootstrap must NOT add
        // a silent admin on top — the manual user is the only
        // signal "this database is in use".
        let (store, _g) = fresh_store().await;
        let pw_hash = hash_password("manual-user-pw-12345").unwrap();
        store
            .create_user(&NewUser {
                username: "viewer1",
                role: Role::Viewer,
                password_hash: Some(&pw_hash),
                oidc_subject_hash: None,
                force_password_reset: false,
            })
            .await
            .unwrap();
        let out = bootstrap_if_needed(&store, AuthMode::Local).await.unwrap();
        assert!(
            matches!(out, BootstrapOutcome::SkippedAlreadyBootstrapped),
            "{out:?}"
        );
    }

    // ----- end-to-end: the printed password actually works ---------------

    #[tokio::test]
    async fn printed_password_verifies_against_stored_hash() {
        // The whole point of the bootstrap flow is the operator
        // can type the WARN'd password into the login form on
        // first boot. Pin that round-trip end-to-end so a future
        // refactor of `hash_password` can't quietly break it.
        let (store, _g) = fresh_store().await;
        let out = bootstrap_if_needed(&store, AuthMode::Local).await.unwrap();
        let one_time_password = match out {
            BootstrapOutcome::AdminCreated {
                one_time_password, ..
            } => one_time_password,
            other => panic!("{other:?}"),
        };
        let (_user_id, phc) = store
            .get_password_hash_for_login(BOOTSTRAP_USERNAME)
            .await
            .unwrap()
            .expect("admin login row");
        assert!(super::super::passwords::verify_password(
            &one_time_password,
            &phc
        ));
    }
}
