//! M6 — local user authentication primitives.
//!
//! Pure, self-contained sub-modules consumed by the
//! `auth::login` handler set:
//!
//! * [`passwords`] — argon2id hashing + password policy
//!   (Phase 2 Step 2.2).
//! * [`lockout`] — failed-login lockout FSM
//!   (Phase 2 Step 2.3).
//! * [`sessions`] — HS256 access JWT + opaque refresh-secret
//!   primitives (Phase 2 Step 2.4).
//! * [`require_role`] — axum extractor that pulls the session
//!   off the request, decodes the JWT, and asserts a minimum
//!   role. Bridges the legacy `AdminClaims` shape during the
//!   deprecation window (Phase 2 Step 2.5).
//! * [`bootstrap`] — first-boot admin provisioning. Runs once
//!   when `auth.mode` allows local users AND the `users` table
//!   is empty (Phase 2 Step 2.6).
//! * [`login`] — the four HTTP handlers (`/login`, `/refresh`,
//!   `/logout`, `/change-password`) that compose every other
//!   primitive in this module (Phase 2 Step 2.7).
//! * [`users_admin`] — six HTTP handlers
//!   (`GET/POST /api/v1/admin/users`, `PUT/DELETE
//!   /api/v1/admin/users/:id`, `POST .../reset-password`,
//!   `POST .../unlock`) that let an admin manage the local-user
//!   roster (Phase 2 Step 2.8).
//! * [`oidc`] — OIDC discovery + JWKS cache + ID-token
//!   verification primitives. Backs the Step 3.3 auth-code
//!   flow handler (Phase 3 Step 3.1).
//!
//! Future siblings (planned in [`docs/M6_IDENTITY.md`](../../../docs/M6_IDENTITY.md)):
//!
//! * (none — Phase 3 lands `oidc::role_map` + the auth-code
//!   handler in upcoming sub-steps and they live inside the
//!   `oidc` module.)
//!
//! Keeping each concern as a tiny leaf module under `auth/`
//! lets the login handler set compose them without pulling in
//! a god-module.

pub mod bootstrap;
pub mod lockout;
pub mod login;
// `oidc` lands ahead of its in-tree consumers (Step 3.3 wires
// the auth-code handler + Step 3.2 the role mapper). Allow the
// dead code so the parallel rollout doesn't fail clippy; this
// allow disappears the moment Step 3.3 imports `OidcClient`.
#[allow(dead_code)]
pub mod oidc;
pub mod passwords;
pub mod require_role;
pub mod sessions;
pub mod users_admin;
