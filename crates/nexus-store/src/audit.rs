//! M6 audit log: row types, store CRUD, and the
//! `record_audit_event` helper.
//!
//! The shapes here cross the store boundary into `nexus-engine`'s
//! HTTP handlers and the eventual `/admin/audit` admin UI. They do
//! NOT cross the wire as-is — admin handlers project them into the
//! TypeScript types under `ui/src/api/types/` after stripping any
//! secret material the caller chose to write.
//!
//! Three core operations:
//!
//! 1. [`Store::record_audit_event`] — writes one row inside a
//!    caller-supplied [`sqlx::Transaction`]. The expected use is
//!    "open tx → mutate domain table → record audit row → commit".
//!    A failed commit rolls back both halves atomically, which is
//!    the entire reason this API takes a transaction rather than
//!    opening its own.
//!
//! 2. [`Store::record_audit_event_standalone`] — opens its own tx
//!    for events that have no companion domain mutation: login
//!    success/failure, password change, lockout trip, retention
//!    sweep, etc.
//!
//! 3. [`Store::list_audit_filtered`] +
//!    [`Store::list_audit_for_resource`] — read paths for the
//!    `/admin/audit` global view and the per-resource history
//!    panels.
//!
//! The action verbs are free-form strings (not a CHECK enum) so
//! new resources don't require a schema migration. The convention
//! is `<resource>.<verb>` (`camera.update`, `user.delete`,
//! `login.success`, `login.failure`, `login.locked`,
//! `user.password_changed`, etc.). The engine enforces it in code.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, Transaction};

use crate::{Store, StoreError};

/// What kind of principal performed the action. Wire shape is
/// `"local_user" | "oidc_user" | "dev_token" | "system"` — see
/// the CHECK in migration `0009_audit_log.sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditActorKind {
    /// M6 local-users backend.
    LocalUser,
    /// M6 OIDC backend.
    OidcUser,
    /// Legacy M-Install Checkpoint 2 shared bearer. Will be
    /// removed when `--features prod-auth` graduates the auth
    /// stack — until then, every dev_token-backed write still
    /// lands an audit row with this actor_kind.
    DevToken,
    /// Engine-internal action (boot bootstrap, retention sweeper,
    /// background tasks).
    System,
}

impl AuditActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditActorKind::LocalUser => "local_user",
            AuditActorKind::OidcUser => "oidc_user",
            AuditActorKind::DevToken => "dev_token",
            AuditActorKind::System => "system",
        }
    }
}

impl TryFrom<&str> for AuditActorKind {
    type Error = String;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "local_user" => Ok(AuditActorKind::LocalUser),
            "oidc_user" => Ok(AuditActorKind::OidcUser),
            "dev_token" => Ok(AuditActorKind::DevToken),
            "system" => Ok(AuditActorKind::System),
            other => Err(format!("unknown AuditActorKind: {other:?}")),
        }
    }
}

/// Terminal outcome of the recorded action. Pinned at 3 values so
/// aggregations (`SELECT COUNT(*) FROM audit_log WHERE outcome =
/// 'failure'`) can group cheaply. See the CHECK in migration
/// `0009_audit_log.sql`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditOutcome {
    /// The action completed and the underlying mutation (if any)
    /// committed. Login success also uses this.
    #[default]
    Success,
    /// The action was attempted but failed at the application
    /// layer (validation error, DB constraint violation, bad
    /// password, etc.). The companion `before_json` and
    /// `after_json` are usually absent.
    Failure,
    /// The action was rejected at the authorisation layer (403 /
    /// 401 / lockout trip). Distinguishing this from `Failure`
    /// lets dashboards surface "someone tried to delete a user
    /// without the admin role" separately from "the delete
    /// succeeded".
    Denied,
}

impl AuditOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditOutcome::Success => "success",
            AuditOutcome::Failure => "failure",
            AuditOutcome::Denied => "denied",
        }
    }
}

impl TryFrom<&str> for AuditOutcome {
    type Error = String;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "success" => Ok(AuditOutcome::Success),
            "failure" => Ok(AuditOutcome::Failure),
            "denied" => Ok(AuditOutcome::Denied),
            other => Err(format!("unknown AuditOutcome: {other:?}")),
        }
    }
}

/// What the caller hands to [`Store::record_audit_event`]. All
/// optional fields default to `None` so the most common cases
/// (no IP / UA / before-after) need only the three required
/// fields.
#[derive(Debug, Clone, Default)]
pub struct NewAuditEntry<'a> {
    pub actor_kind: Option<AuditActorKind>,
    /// `None` for system actions; `Some(<stringified user id>)`
    /// or `Some(<hashed oidc subject>)` for human actors.
    pub actor_id: Option<&'a str>,
    /// Human-readable label for the actor *at the time of action*.
    /// E.g. the user's username (local) or `preferred_username`
    /// claim (OIDC). Denormalised so renames + deletes leave
    /// history intact.
    pub actor_label: &'a str,
    pub action: &'a str,
    pub resource_kind: Option<&'a str>,
    pub resource_id: Option<&'a str>,
    pub before_json: Option<&'a str>,
    pub after_json: Option<&'a str>,
    pub outcome: AuditOutcome,
    pub ip: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

impl<'a> NewAuditEntry<'a> {
    /// Builder shorthand for a successful login. The handler still
    /// supplies `actor_id`, IP, and user agent.
    pub fn login_success(actor_kind: AuditActorKind, actor_label: &'a str) -> Self {
        Self {
            actor_kind: Some(actor_kind),
            actor_label,
            action: "login.success",
            outcome: AuditOutcome::Success,
            ..Default::default()
        }
    }

    /// Builder shorthand for a rejected login (bad password OR
    /// disabled OR locked — the handler does NOT distinguish in
    /// the audit row, to mirror the 401-deliberately-ambiguous
    /// response).
    pub fn login_failure(actor_label: &'a str) -> Self {
        Self {
            actor_kind: Some(AuditActorKind::LocalUser),
            actor_label,
            action: "login.failure",
            outcome: AuditOutcome::Failure,
            ..Default::default()
        }
    }
}

/// What we hand back from `list_*` calls. `created_at` is parsed
/// from the SQLite `TEXT` storage to a typed `DateTime<Utc>` so
/// downstream handlers (and the UI projection) don't have to
/// re-parse it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    pub id: i64,
    pub actor_kind: AuditActorKind,
    pub actor_id: Option<String>,
    pub actor_label: String,
    pub action: String,
    pub resource_kind: Option<String>,
    pub resource_id: Option<String>,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub outcome: AuditOutcome,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Optional filters for the global audit feed.
/// Each `Some(...)` adds a `AND col = ?` clause; `None` is
/// ignored. The `since` / `until` window uses inclusive bounds
/// (`>=` and `<=`).
#[derive(Debug, Clone, Default)]
pub struct AuditFilter<'a> {
    pub actor_id: Option<&'a str>,
    pub action: Option<&'a str>,
    pub resource_kind: Option<&'a str>,
    pub resource_id: Option<&'a str>,
    pub outcome: Option<AuditOutcome>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

impl Store {
    /// Insert one `audit_log` row inside the caller's transaction.
    /// Used by every privileged mutation handler: open the tx,
    /// mutate the domain table, call `record_audit_event`, commit.
    /// If the commit fails, both halves roll back atomically.
    ///
    /// `entry.actor_kind` defaults to [`AuditActorKind::System`]
    /// when `None`. The handler should set it explicitly for any
    /// principal-driven write; the default only exists so engine-
    /// internal callers (retention sweeper, etc.) can use the
    /// `Default` builder.
    pub async fn record_audit_event(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        entry: &NewAuditEntry<'_>,
    ) -> Result<i64, StoreError> {
        let actor_kind = entry.actor_kind.unwrap_or(AuditActorKind::System).as_str();
        let outcome = entry.outcome.as_str();
        // Use sqlx::query_scalar so we get back the generated
        // rowid in one round-trip.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO audit_log (
                actor_kind, actor_id, actor_label,
                action, resource_kind, resource_id,
                before_json, after_json, outcome,
                ip, user_agent
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING id",
        )
        .bind(actor_kind)
        .bind(entry.actor_id)
        .bind(entry.actor_label)
        .bind(entry.action)
        .bind(entry.resource_kind)
        .bind(entry.resource_id)
        .bind(entry.before_json)
        .bind(entry.after_json)
        .bind(outcome)
        .bind(entry.ip)
        .bind(entry.user_agent)
        .fetch_one(&mut **tx)
        .await?;
        Ok(id)
    }

    /// Insert one `audit_log` row inside its own transaction. Use
    /// for events that have no companion domain mutation — login
    /// attempts, lockout trips, password changes, OIDC callbacks,
    /// boot-time bootstrap, retention sweeper. Returns the new
    /// `audit_log.id`.
    pub async fn record_audit_event_standalone(
        &self,
        entry: &NewAuditEntry<'_>,
    ) -> Result<i64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let id = self.record_audit_event(&mut tx, entry).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Last `limit` audit rows for a given resource, newest first.
    /// Hits `idx_audit_resource`. Drives the per-resource history
    /// panel on every detail view.
    pub async fn list_audit_for_resource(
        &self,
        resource_kind: &str,
        resource_id: &str,
        limit: i64,
    ) -> Result<Vec<AuditEntry>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, actor_kind, actor_id, actor_label, action,
                    resource_kind, resource_id, before_json, after_json,
                    outcome, ip, user_agent, created_at
             FROM audit_log
             WHERE resource_kind = ? AND resource_id = ?
             ORDER BY created_at DESC, id DESC
             LIMIT ?",
        )
        .bind(resource_kind)
        .bind(resource_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_audit_row).collect()
    }

    /// Filtered global audit feed for the `/admin/audit` table.
    /// `limit` caps the page size; `offset` pages further. Newest
    /// first.
    pub async fn list_audit_filtered(
        &self,
        filter: &AuditFilter<'_>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AuditEntry>, StoreError> {
        // Build the SQL incrementally so each optional filter
        // contributes one parameter. We bind in the same order
        // we add clauses to keep `$n` placeholders aligned with
        // the `query_with` argument order.
        let mut sql = String::from(
            "SELECT id, actor_kind, actor_id, actor_label, action,
                    resource_kind, resource_id, before_json, after_json,
                    outcome, ip, user_agent, created_at
             FROM audit_log
             WHERE 1=1",
        );
        if filter.actor_id.is_some() {
            sql.push_str(" AND actor_id = ?");
        }
        if filter.action.is_some() {
            sql.push_str(" AND action = ?");
        }
        if filter.resource_kind.is_some() {
            sql.push_str(" AND resource_kind = ?");
        }
        if filter.resource_id.is_some() {
            sql.push_str(" AND resource_id = ?");
        }
        if filter.outcome.is_some() {
            sql.push_str(" AND outcome = ?");
        }
        if filter.since.is_some() {
            sql.push_str(" AND created_at >= ?");
        }
        if filter.until.is_some() {
            sql.push_str(" AND created_at <= ?");
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?");

        let mut q = sqlx::query(&sql);
        if let Some(v) = filter.actor_id {
            q = q.bind(v);
        }
        if let Some(v) = filter.action {
            q = q.bind(v);
        }
        if let Some(v) = filter.resource_kind {
            q = q.bind(v);
        }
        if let Some(v) = filter.resource_id {
            q = q.bind(v);
        }
        if let Some(v) = filter.outcome {
            q = q.bind(v.as_str());
        }
        if let Some(v) = filter.since {
            q = q.bind(v.to_rfc3339());
        }
        if let Some(v) = filter.until {
            q = q.bind(v.to_rfc3339());
        }
        q = q.bind(limit).bind(offset);

        let rows = q.fetch_all(&self.pool).await?;
        rows.into_iter().map(decode_audit_row).collect()
    }

    /// Delete every audit_log row whose `created_at` is strictly
    /// older than `cutoff`. Returns the affected-row count so the
    /// retention sweeper can log it. Used by the daily sweeper
    /// hook (`runtime.audit.retention_days`).
    pub async fn delete_audit_older_than(&self, cutoff: DateTime<Utc>) -> Result<u64, StoreError> {
        let res = sqlx::query("DELETE FROM audit_log WHERE created_at < ?")
            .bind(cutoff.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}

fn decode_audit_row(row: sqlx::sqlite::SqliteRow) -> Result<AuditEntry, StoreError> {
    let actor_kind_str: String = row.try_get("actor_kind")?;
    let outcome_str: String = row.try_get("outcome")?;
    let created_at_str: String = row.try_get("created_at")?;

    let actor_kind =
        AuditActorKind::try_from(actor_kind_str.as_str()).map_err(StoreError::Decode)?;
    let outcome = AuditOutcome::try_from(outcome_str.as_str()).map_err(StoreError::Decode)?;
    let created_at = parse_sqlite_ts(&created_at_str)?;

    Ok(AuditEntry {
        id: row.try_get("id")?,
        actor_kind,
        actor_id: row.try_get("actor_id")?,
        actor_label: row.try_get("actor_label")?,
        action: row.try_get("action")?,
        resource_kind: row.try_get("resource_kind")?,
        resource_id: row.try_get("resource_id")?,
        before_json: row.try_get("before_json")?,
        after_json: row.try_get("after_json")?,
        outcome,
        ip: row.try_get("ip")?,
        user_agent: row.try_get("user_agent")?,
        created_at,
    })
}

/// `audit_log.created_at` stores ISO-8601 UTC with millisecond
/// precision (`2026-05-18T14:32:01.123Z`) — see the strftime
/// DEFAULT in migration `0009_audit_log.sql`. Caller-written
/// timestamps go in via `DateTime::<Utc>::to_rfc3339()`
/// (`...+00:00`). For belt-and-braces, also accept the legacy
/// SQLite `CURRENT_TIMESTAMP` form (`YYYY-MM-DD HH:MM:SS`) in
/// case some operational tool inserts a row directly.
fn parse_sqlite_ts(s: &str) -> Result<DateTime<Utc>, StoreError> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|naive| naive.and_utc())
        .map_err(|e| StoreError::Decode(format!("bad audit_log.created_at {s:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_kind_round_trip() {
        for k in [
            AuditActorKind::LocalUser,
            AuditActorKind::OidcUser,
            AuditActorKind::DevToken,
            AuditActorKind::System,
        ] {
            assert_eq!(AuditActorKind::try_from(k.as_str()).unwrap(), k);
        }
        assert!(AuditActorKind::try_from("ghost").is_err());
    }

    #[test]
    fn outcome_round_trip() {
        for o in [
            AuditOutcome::Success,
            AuditOutcome::Failure,
            AuditOutcome::Denied,
        ] {
            assert_eq!(AuditOutcome::try_from(o.as_str()).unwrap(), o);
        }
        assert!(AuditOutcome::try_from("partial").is_err());
    }

    #[test]
    fn login_success_builder_sets_action_and_outcome() {
        let e = NewAuditEntry::login_success(AuditActorKind::LocalUser, "alice");
        assert_eq!(e.action, "login.success");
        assert_eq!(e.outcome, AuditOutcome::Success);
        assert_eq!(e.actor_label, "alice");
        assert_eq!(e.actor_kind, Some(AuditActorKind::LocalUser));
    }

    #[test]
    fn login_failure_builder_uses_local_actor() {
        let e = NewAuditEntry::login_failure("eve");
        assert_eq!(e.action, "login.failure");
        assert_eq!(e.outcome, AuditOutcome::Failure);
        // A bad password against an OIDC-only deployment would
        // never reach the local-login path; this builder hard-
        // codes LocalUser. The OIDC backend writes its own
        // failure rows with the right actor_kind.
        assert_eq!(e.actor_kind, Some(AuditActorKind::LocalUser));
    }

    #[test]
    fn sqlite_native_timestamp_parses_as_utc() {
        let parsed = parse_sqlite_ts("2026-05-18 14:32:01").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-05-18T14:32:01+00:00");
    }

    #[test]
    fn rfc3339_timestamp_round_trips() {
        let parsed = parse_sqlite_ts("2026-05-18T14:32:01Z").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-05-18T14:32:01+00:00");
    }
}
