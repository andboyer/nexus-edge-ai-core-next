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
//!
//! Future siblings (planned in [`docs/M6_IDENTITY.md`](../../../docs/M6_IDENTITY.md)):
//!
//! * `users_admin` — `GET/POST/PUT/DELETE /api/v1/admin/users`
//!   + the unlock + reset-password actions (Phase 2 Step 2.8).
//! * `oidc` — Phase 3.
//!
//! Keeping each concern as a tiny leaf module under `auth/`
//! lets the login handler set compose them without pulling in
//! a god-module.

pub mod bootstrap;
pub mod lockout;
pub mod login;
pub mod passwords;
pub mod require_role;
pub mod sessions;
