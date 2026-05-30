//! Per-camera `(track_id, entity_local_id)` persistence.
//!
//! Phase 5.6 · R4. The pipeline-side
//! [`SightingScheduler`](../../../nexus-pipeline/src/entity_sighting.rs)
//! mints a UUIDv7 `entity_local_id` per stable track. The mapping is
//! kept in-memory; if the engine crashes and systemd restarts it
//! within the tracker re-association window (~30 s), the tracker may
//! re-issue the same `(camera_id, track_id)` for what is still the
//! same physical object — without persistence we would mint a fresh
//! `entity_local_id` and double-count the entity until the cloud-side
//! linker corrects via embedding similarity.
//!
//! This module owns the engine-side read/write surface for the
//! `entity_local_state` table (migration `0018_entity_local_state.sql`).
//! Callers:
//!
//! * `nexus-engine` boot: `load_recent_entity_locals(window)` to
//!   pre-seed every per-camera scheduler with rows that fall inside
//!   the re-association window.
//! * `nexus-engine` `cloud_sighting` worker: `upsert_entity_local`
//!   on every `is_first` snapshot.
//! * `nexus-engine` periodic sweeper: `prune_entity_local_state`
//!   on a wall-clock tick (default hourly) to keep the table small.
//!
//! ### Hard PII invariant — DO NOT ADD COLUMNS WITHOUT WEDGE-PLAN REVIEW
//!
//! Per [`AGENTS.md`](../../../AGENTS.md) rule 7 + the wedge plan's
//! pseudonymous-by-default substrate: the row carries only
//! `(camera_id, track_id, entity_local_id, started_ts, last_seen_at)`.
//! It MUST NOT carry `name`, `email`, `phone`, `face_*`, or any other
//! personal identifier alongside `entity_local_id`. If the M6 admin
//! surface ever lets an operator attach a label to an entity, that
//! label lives in a separate `operator_entity_labels` table that
//! NEVER replicates to the cloud.

use chrono::{DateTime, Utc};
use nexus_types::{CameraId, TrackId};
use sqlx::Row as _;

use crate::{Store, StoreError};

/// One row of `entity_local_state`. The composite key
/// `(camera_id, track_id)` is unique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityLocalRecord {
    pub camera_id: CameraId,
    pub track_id: TrackId,
    pub entity_local_id: String,
    pub started_ts: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

/// Upsert payload. `started_ts` is only honored on INSERT — the
/// scheduler stamps it at the first emit and never moves it
/// afterwards. `last_seen_at` is always overwritten with the value
/// supplied here.
#[derive(Debug, Clone)]
pub struct EntityLocalUpsert {
    pub camera_id: CameraId,
    pub track_id: TrackId,
    pub entity_local_id: String,
    pub started_ts: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

impl Store {
    /// Insert or update a `(camera_id, track_id)` row. `started_ts`
    /// is only set on INSERT; `last_seen_at` and `entity_local_id`
    /// are always overwritten with the supplied values.
    ///
    /// `entity_local_id` overwrite is intentional and defensive: if
    /// the scheduler somehow lost its in-memory entry but the DB row
    /// is stale-but-present, the next first-emit will refresh both
    /// the id and the timestamps so the next boot reads consistent
    /// state.
    pub async fn upsert_entity_local(&self, u: &EntityLocalUpsert) -> Result<(), StoreError> {
        sqlx::query(
            r#"
            INSERT INTO entity_local_state
                (camera_id, track_id, entity_local_id, started_ts, last_seen_at)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT (camera_id, track_id) DO UPDATE SET
                entity_local_id = excluded.entity_local_id,
                last_seen_at    = excluded.last_seen_at
            "#,
        )
        .bind(u.camera_id)
        .bind(i64::try_from(u.track_id).unwrap_or(i64::MAX))
        .bind(&u.entity_local_id)
        .bind(u.started_ts.to_rfc3339())
        .bind(u.last_seen_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load every row whose `last_seen_at >= since`. Intended for
    /// boot-time hydration of the per-camera schedulers; `since` is
    /// typically `now - 2 * track_gc_after` to cover even a
    /// pessimistically slow systemd restart.
    pub async fn load_recent_entity_locals(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<EntityLocalRecord>, StoreError> {
        let rows = sqlx::query(
            r#"
            SELECT camera_id, track_id, entity_local_id, started_ts, last_seen_at
              FROM entity_local_state
             WHERE last_seen_at >= ?
            "#,
        )
        .bind(since.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let camera_id: i64 = row.try_get("camera_id")?;
            let track_id_i: i64 = row.try_get("track_id")?;
            let entity_local_id: String = row.try_get("entity_local_id")?;
            let started_ts: String = row.try_get("started_ts")?;
            let last_seen_at: String = row.try_get("last_seen_at")?;
            let started_ts = DateTime::parse_from_rfc3339(&started_ts)
                .map_err(|e| StoreError::Decode(format!("started_ts: {e}")))?
                .with_timezone(&Utc);
            let last_seen_at = DateTime::parse_from_rfc3339(&last_seen_at)
                .map_err(|e| StoreError::Decode(format!("last_seen_at: {e}")))?
                .with_timezone(&Utc);
            out.push(EntityLocalRecord {
                camera_id,
                track_id: u64::try_from(track_id_i).unwrap_or(0),
                entity_local_id,
                started_ts,
                last_seen_at,
            });
        }
        Ok(out)
    }

    /// Delete every row whose `last_seen_at < older_than`. Returns
    /// the number of rows pruned (purely informational — callers
    /// log it as a debug counter and do not gate behaviour on it).
    pub async fn prune_entity_local_state(
        &self,
        older_than: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let res = sqlx::query("DELETE FROM entity_local_state WHERE last_seen_at < ?")
            .bind(older_than.to_rfc3339())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Per-camera variant of [`Self::load_recent_entity_locals`].
    /// Used by the reconciler hot-add path so we don't pull rows
    /// for unrelated cameras on every camera add / URL-change
    /// restart.
    pub async fn load_recent_entity_locals_for_camera(
        &self,
        camera_id: CameraId,
        since: DateTime<Utc>,
    ) -> Result<Vec<EntityLocalRecord>, StoreError> {
        let rows = sqlx::query(
            r#"
            SELECT camera_id, track_id, entity_local_id, started_ts, last_seen_at
              FROM entity_local_state
             WHERE camera_id = ? AND last_seen_at >= ?
            "#,
        )
        .bind(camera_id)
        .bind(since.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let camera_id: i64 = row.try_get("camera_id")?;
            let track_id_i: i64 = row.try_get("track_id")?;
            let entity_local_id: String = row.try_get("entity_local_id")?;
            let started_ts: String = row.try_get("started_ts")?;
            let last_seen_at: String = row.try_get("last_seen_at")?;
            let started_ts = DateTime::parse_from_rfc3339(&started_ts)
                .map_err(|e| StoreError::Decode(format!("started_ts: {e}")))?
                .with_timezone(&Utc);
            let last_seen_at = DateTime::parse_from_rfc3339(&last_seen_at)
                .map_err(|e| StoreError::Decode(format!("last_seen_at: {e}")))?
                .with_timezone(&Utc);
            out.push(EntityLocalRecord {
                camera_id,
                track_id: u64::try_from(track_id_i).unwrap_or(0),
                entity_local_id,
                started_ts,
                last_seen_at,
            });
        }
        Ok(out)
    }

    /// Delete a single `(camera_id, track_id)` row. Used by the
    /// scheduler's GC sweep so the table doesn't accumulate rows
    /// for tracks the in-memory state has already dropped. The
    /// periodic sweeper is the safety net.
    pub async fn delete_entity_local(
        &self,
        camera_id: CameraId,
        track_id: TrackId,
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM entity_local_state WHERE camera_id = ? AND track_id = ?")
            .bind(camera_id)
            .bind(i64::try_from(track_id).unwrap_or(i64::MAX))
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use nexus_config::StoreConfig;
    use tempfile::TempDir;

    async fn fresh_store() -> (Store, TempDir) {
        let tmp = TempDir::new().expect("tempdir");
        let cfg = StoreConfig {
            url: format!("sqlite://{}/store.db?mode=rwc", tmp.path().display()),
            ..StoreConfig::default()
        };
        let store = Store::open(&cfg).await.expect("store open");
        (store, tmp)
    }

    fn upsert(
        camera_id: CameraId,
        track_id: TrackId,
        entity_local_id: &str,
        started_ts: DateTime<Utc>,
        last_seen_at: DateTime<Utc>,
    ) -> EntityLocalUpsert {
        EntityLocalUpsert {
            camera_id,
            track_id,
            entity_local_id: entity_local_id.to_string(),
            started_ts,
            last_seen_at,
        }
    }

    #[tokio::test]
    async fn upsert_then_load_roundtrip() {
        let (store, _tmp) = fresh_store().await;
        let now = Utc::now();
        store
            .upsert_entity_local(&upsert(1, 42, "eid-a", now, now))
            .await
            .unwrap();
        let rows = store
            .load_recent_entity_locals(now - Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].camera_id, 1);
        assert_eq!(rows[0].track_id, 42);
        assert_eq!(rows[0].entity_local_id, "eid-a");
    }

    #[tokio::test]
    async fn upsert_preserves_started_ts_updates_last_seen() {
        let (store, _tmp) = fresh_store().await;
        let t0 = Utc::now();
        let t1 = t0 + Duration::seconds(5);
        store
            .upsert_entity_local(&upsert(1, 42, "eid-a", t0, t0))
            .await
            .unwrap();
        // Re-upsert with a fresh last_seen_at but a different
        // (later) started_ts param — the ON CONFLICT update branch
        // ignores `excluded.started_ts` and keeps the original.
        store
            .upsert_entity_local(&upsert(1, 42, "eid-a", t1, t1))
            .await
            .unwrap();
        let rows = store
            .load_recent_entity_locals(t0 - Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].started_ts.timestamp_millis(),
            t0.timestamp_millis(),
            "started_ts is INSERT-only"
        );
        assert_eq!(
            rows[0].last_seen_at.timestamp_millis(),
            t1.timestamp_millis(),
            "last_seen_at is overwritten"
        );
    }

    #[tokio::test]
    async fn load_recent_respects_since_filter() {
        let (store, _tmp) = fresh_store().await;
        let now = Utc::now();
        let stale = now - Duration::hours(2);
        let fresh = now - Duration::seconds(10);
        store
            .upsert_entity_local(&upsert(1, 1, "stale", stale, stale))
            .await
            .unwrap();
        store
            .upsert_entity_local(&upsert(1, 2, "fresh", fresh, fresh))
            .await
            .unwrap();
        let rows = store
            .load_recent_entity_locals(now - Duration::minutes(1))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_local_id, "fresh");
    }

    #[tokio::test]
    async fn prune_drops_only_old_rows() {
        let (store, _tmp) = fresh_store().await;
        let now = Utc::now();
        let stale = now - Duration::hours(2);
        let fresh = now - Duration::seconds(10);
        store
            .upsert_entity_local(&upsert(1, 1, "stale", stale, stale))
            .await
            .unwrap();
        store
            .upsert_entity_local(&upsert(1, 2, "fresh", fresh, fresh))
            .await
            .unwrap();
        let pruned = store
            .prune_entity_local_state(now - Duration::minutes(1))
            .await
            .unwrap();
        assert_eq!(pruned, 1);
        let rows = store
            .load_recent_entity_locals(stale - Duration::seconds(1))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity_local_id, "fresh");
    }

    #[tokio::test]
    async fn multi_camera_isolation() {
        let (store, _tmp) = fresh_store().await;
        let now = Utc::now();
        // Same track_id on two different cameras is distinct rows.
        store
            .upsert_entity_local(&upsert(1, 7, "cam1-track7", now, now))
            .await
            .unwrap();
        store
            .upsert_entity_local(&upsert(2, 7, "cam2-track7", now, now))
            .await
            .unwrap();
        let rows = store
            .load_recent_entity_locals(now - Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        let mut ids: Vec<_> = rows.iter().map(|r| r.entity_local_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["cam1-track7", "cam2-track7"]);
    }
}
