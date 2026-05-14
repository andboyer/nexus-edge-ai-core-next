//! Motion timeline & clip recording — the M2.1 storage surface.
//!
//! Two tables back this module:
//!
//! * `motion_clips` — one row per recorded clip, identified by an
//!   integer id and a path relative to `RuntimeConfig.clips.clips_dir`.
//! * `motion_events` — many-rows-per-clip; the per-track lifecycle
//!   timeline (`born` / `updated` / `died`) keyed by `(camera_id,
//!   captured_at)`.
//!
//! All FKs are `ON DELETE CASCADE` (see `0002_motion_clips.sql`), so
//! eviction is a single `DELETE motion_clips WHERE id = ?`. The
//! `cascade_delete_clip_metadata` helper here is the explicit form
//! kept for the FK-disabled fallback path called out in
//! `docs/M2_STORAGE.md`.

use chrono::{DateTime, Utc};
use nexus_types::{BBox, CameraId, TrackId};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{Store, StoreError};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub type ClipId = i64;
pub type MotionEventId = i64;

/// Lifecycle event kind written into `motion_events.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionEventKind {
    Born,
    Updated,
    Died,
}

impl MotionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MotionEventKind::Born => "born",
            MotionEventKind::Updated => "updated",
            MotionEventKind::Died => "died",
        }
    }

    pub fn parse(s: &str) -> Result<Self, StoreError> {
        match s {
            "born" => Ok(Self::Born),
            "updated" => Ok(Self::Updated),
            "died" => Ok(Self::Died),
            other => Err(StoreError::NotFound(format!("motion_event.kind={other}"))),
        }
    }
}

/// Args for opening a new `motion_clips` row.
#[derive(Debug, Clone)]
pub struct NewClip {
    pub camera_id: CameraId,
    pub started_at: DateTime<Utc>,
    pub path: String,
    pub codec: String,
    pub container: String,
    pub backend_id: String,
}

/// Args for closing an in-progress `motion_clips` row.
#[derive(Debug, Clone)]
pub struct ClipClose {
    pub ended_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub size_bytes: i64,
    /// Optional rename. M2.1 spec requires the on-disk filename to
    /// include `duration_ms` (e.g. `{start_unix_ms}_{duration_ms}.mp4`)
    /// which is only known at close time. The recorder renames the
    /// in-flight file then sets this so the DB pointer stays valid.
    /// `None` means "leave `path` unchanged" (Stage A stub recorder
    /// when no rename is needed).
    pub path: Option<String>,
}

/// Hydrated `motion_clips` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipRow {
    pub id: ClipId,
    pub camera_id: CameraId,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub path: String,
    pub duration_ms: i64,
    pub size_bytes: i64,
    pub codec: String,
    pub container: String,
    pub backend_id: String,
}

/// Per-camera clip-occupancy snapshot for the storage health endpoint.
///
/// Returned by [`Store::per_camera_clip_stats`]. Cameras with zero
/// clips are NOT in the result; the API handler may zero-fill if it
/// wants a row per known camera.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerCameraClipStats {
    pub camera_id: CameraId,
    /// Number of `motion_clips` rows owned by this camera.
    pub clip_count: i64,
    /// `SUM(size_bytes)` across those rows. In-flight clips
    /// (size_bytes still 0) contribute 0 — eventually correct
    /// once `close_clip` stamps the finalised file size.
    pub bytes: i64,
    /// `MIN(started_at)` across those rows. Drives "retention
    /// horizon for camera X" in the UI.
    pub oldest_started_at: DateTime<Utc>,
}

/// Args for writing a single `motion_events` row.
#[derive(Debug, Clone)]
pub struct NewMotionEvent {
    pub camera_id: CameraId,
    pub clip_id: ClipId,
    pub track_id: TrackId,
    pub kind: MotionEventKind,
    pub captured_at: DateTime<Utc>,
    pub bbox: BBox,
    pub label: String,
    pub confidence: f32,
    /// Free-form annotator output (motion.speed_class, group.size, etc).
    pub attributes_json: String,
}

/// Hydrated `motion_events` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionEventRow {
    pub id: MotionEventId,
    pub camera_id: CameraId,
    pub clip_id: ClipId,
    pub track_id: TrackId,
    pub kind: MotionEventKind,
    pub captured_at: DateTime<Utc>,
    pub bbox: BBox,
    pub label: String,
    pub confidence: f32,
    pub attributes_json: String,
}

/// One time-bucket aggregation of motion activity for a camera.
/// Returned by [`Store::list_motion_histogram_for_camera`] and
/// surfaced to the UI as the per-hour Timeline grid bars.
///
/// Buckets are sparse — empty intervals are NOT included; the UI
/// fills zeros client-side. `bucket_start` is the inclusive lower
/// edge of the bucket, computed as `from + bucket_seconds * bucket`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionHistogramBucket {
    /// Zero-based bucket index counted from `from`. The UI uses
    /// this for grid layout without re-deriving timestamps.
    pub bucket: i64,
    /// Inclusive start time of this bucket
    /// (`from + bucket_seconds * bucket`).
    pub bucket_start: DateTime<Utc>,
    /// Number of `motion_events` rows whose `captured_at` falls in
    /// the bucket. Drives the bar height.
    pub event_count: i64,
    /// Number of distinct `clip_id` values referenced by those
    /// events. Drives the secondary clip-count badge on hover.
    pub clip_count: i64,
}

// ---------------------------------------------------------------------------
// Store impl block — one new method per repo operation.
// ---------------------------------------------------------------------------

impl Store {
    /// Insert a new clip row in the `recording` state. `ended_at` /
    /// `duration_ms` / `size_bytes` are populated by `close_clip` when the
    /// recorder finalises the file.
    pub async fn open_clip(&self, new: &NewClip) -> Result<ClipId, StoreError> {
        let row = sqlx::query(
            "INSERT INTO motion_clips
                 (camera_id, started_at, path, codec, container, backend_id)
             VALUES (?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(new.camera_id)
        .bind(new.started_at.to_rfc3339())
        .bind(&new.path)
        .bind(&new.codec)
        .bind(&new.container)
        .bind(&new.backend_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    /// Stamp the close metadata on an in-progress clip. Optionally
    /// updates `path` too — set when the recorder renamed the file
    /// from its in-flight name to the final `{start_ms}_{dur_ms}.mp4`
    /// shape per M2.1 spec.
    pub async fn close_clip(&self, clip_id: ClipId, close: &ClipClose) -> Result<(), StoreError> {
        let res = if let Some(new_path) = &close.path {
            sqlx::query(
                "UPDATE motion_clips
                    SET ended_at = ?, duration_ms = ?, size_bytes = ?, path = ?
                  WHERE id = ?",
            )
            .bind(close.ended_at.to_rfc3339())
            .bind(close.duration_ms)
            .bind(close.size_bytes)
            .bind(new_path)
            .bind(clip_id)
            .execute(&self.pool)
            .await?
        } else {
            sqlx::query(
                "UPDATE motion_clips
                    SET ended_at = ?, duration_ms = ?, size_bytes = ?
                  WHERE id = ?",
            )
            .bind(close.ended_at.to_rfc3339())
            .bind(close.duration_ms)
            .bind(close.size_bytes)
            .bind(clip_id)
            .execute(&self.pool)
            .await?
        };
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("motion_clip id={clip_id}")));
        }
        Ok(())
    }

    pub async fn get_clip(&self, clip_id: ClipId) -> Result<Option<ClipRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, camera_id, started_at, ended_at, path, duration_ms,
                    size_bytes, codec, container, backend_id
               FROM motion_clips
              WHERE id = ?",
        )
        .bind(clip_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(clip_row_from_row).transpose()
    }

    /// Oldest (by `started_at`) finished or in-progress clip for a camera.
    /// Used by the per-camera-fair eviction round.
    pub async fn oldest_clip_for_camera(
        &self,
        camera_id: CameraId,
    ) -> Result<Option<ClipRow>, StoreError> {
        let row = sqlx::query(
            "SELECT id, camera_id, started_at, ended_at, path, duration_ms,
                    size_bytes, codec, container, backend_id
               FROM motion_clips
              WHERE camera_id = ?
              ORDER BY started_at ASC
              LIMIT 1",
        )
        .bind(camera_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(clip_row_from_row).transpose()
    }

    /// Distinct camera ids that currently own at least one clip. The
    /// eviction sampler iterates this set in `id` order so the round
    /// is deterministic across processes.
    pub async fn cameras_with_clips(&self) -> Result<Vec<CameraId>, StoreError> {
        let rows = sqlx::query("SELECT DISTINCT camera_id FROM motion_clips ORDER BY camera_id")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.get::<i64, _>(0)).collect())
    }

    /// Per-camera clip occupancy snapshot. One row per camera that
    /// currently owns at least one clip. Drives the
    /// `per_camera[]` array of `GET /api/v1/storage/local`.
    pub async fn per_camera_clip_stats(&self) -> Result<Vec<PerCameraClipStats>, StoreError> {
        let rows = sqlx::query(
            "SELECT camera_id,
                    COUNT(*)                AS clip_count,
                    COALESCE(SUM(size_bytes), 0) AS bytes,
                    MIN(started_at)         AS oldest_started_at
               FROM motion_clips
              GROUP BY camera_id
              ORDER BY camera_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                let oldest_raw: String = r.get("oldest_started_at");
                let oldest_started_at = DateTime::parse_from_rfc3339(&oldest_raw)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                Ok(PerCameraClipStats {
                    camera_id: r.get::<i64, _>("camera_id"),
                    clip_count: r.get::<i64, _>("clip_count"),
                    bytes: r.get::<i64, _>("bytes"),
                    oldest_started_at,
                })
            })
            .collect()
    }

    /// Every `motion_clips.path` currently in the DB. Used by the
    /// orphan-file scanner to compute "files on disk that have no
    /// matching row" by set difference. Paths are stored relative
    /// to the recorder's `clips_dir`.
    pub async fn known_clip_paths(&self) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query("SELECT path FROM motion_clips")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>(0)).collect())
    }

    /// Single-DELETE eviction. With the `ON DELETE CASCADE` FKs from
    /// `0002_motion_clips.sql` (motion_events) and `0003_events_clip_cascade.sql`
    /// (events.clip_id, flipped from SET NULL to CASCADE in M2.1 closeout),
    /// this also removes every linked `motion_events` row AND every
    /// alert `events` row that referenced the clip — leaving no
    /// half-deleted half-state the M2.1 schema invariant exists to
    /// prevent.
    /// File unlink is the caller's responsibility (see
    /// `docs/M2_STORAGE.md` crash-safety section).
    pub async fn cascade_delete_clip_metadata(&self, clip_id: ClipId) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM motion_clips WHERE id = ?")
            .bind(clip_id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("motion_clip id={clip_id}")));
        }
        Ok(())
    }

    /// Clip rows older than the supplied cutoff, oldest-first. Used by
    /// the daily retention sweeper.
    pub async fn clips_older_than(
        &self,
        cutoff: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<ClipRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, camera_id, started_at, ended_at, path, duration_ms,
                    size_bytes, codec, container, backend_id
               FROM motion_clips
              WHERE started_at < ?
              ORDER BY started_at ASC
              LIMIT ?",
        )
        .bind(cutoff.to_rfc3339())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(clip_row_from_row).collect()
    }

    /// Insert a single motion-event row. The recorder owns the
    /// per-camera batching policy; this method writes one row per call
    /// so callers can pick their own transaction granularity.
    pub async fn insert_motion_event(
        &self,
        ev: &NewMotionEvent,
    ) -> Result<MotionEventId, StoreError> {
        let row = sqlx::query(
            "INSERT INTO motion_events
                 (camera_id, clip_id, track_id, kind, captured_at,
                  bbox_x1, bbox_y1, bbox_x2, bbox_y2,
                  label, confidence, attributes_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(ev.camera_id)
        .bind(ev.clip_id)
        .bind(ev.track_id as i64)
        .bind(ev.kind.as_str())
        .bind(ev.captured_at.to_rfc3339())
        .bind(ev.bbox.x1 as f64)
        .bind(ev.bbox.y1 as f64)
        .bind(ev.bbox.x2 as f64)
        .bind(ev.bbox.y2 as f64)
        .bind(&ev.label)
        .bind(ev.confidence as f64)
        .bind(&ev.attributes_json)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }

    /// Per-camera timeline lookup. Powers the M2.1 API endpoint
    /// `GET /api/v1/cameras/:id/motion?from=&to=`.
    pub async fn list_motion_events_for_camera(
        &self,
        camera_id: CameraId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<MotionEventRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, camera_id, clip_id, track_id, kind, captured_at,
                    bbox_x1, bbox_y1, bbox_x2, bbox_y2,
                    label, confidence, attributes_json
               FROM motion_events
              WHERE camera_id = ? AND captured_at BETWEEN ? AND ?
              ORDER BY captured_at ASC
              LIMIT ?",
        )
        .bind(camera_id)
        .bind(from.to_rfc3339())
        .bind(to.to_rfc3339())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(motion_event_row_from_row).collect()
    }

    /// Bucketed motion-event counts for the per-camera Timeline UI
    /// (M2.1 Stage B B7). Powers
    /// `GET /api/v1/cameras/:id/motion/histogram`.
    ///
    /// Returns sparse buckets — only intervals containing events are
    /// included. The bucket index is computed against `from` so the
    /// UI can position bars deterministically without re-parsing
    /// timestamps. `bucket_seconds` must be > 0 (caller validates).
    pub async fn list_motion_histogram_for_camera(
        &self,
        camera_id: CameraId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        bucket_seconds: i64,
    ) -> Result<Vec<MotionHistogramBucket>, StoreError> {
        debug_assert!(bucket_seconds > 0);
        let from_unix = from.timestamp();
        let rows = sqlx::query(
            "SELECT
                 CAST((strftime('%s', captured_at) - ?) / ? AS INTEGER) AS bucket,
                 COUNT(*) AS event_count,
                 COUNT(DISTINCT clip_id) AS clip_count
               FROM motion_events
              WHERE camera_id = ? AND captured_at BETWEEN ? AND ?
              GROUP BY bucket
              ORDER BY bucket ASC",
        )
        .bind(from_unix)
        .bind(bucket_seconds)
        .bind(camera_id)
        .bind(from.to_rfc3339())
        .bind(to.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let bucket: i64 = row.get("bucket");
                let event_count: i64 = row.get("event_count");
                let clip_count: i64 = row.get("clip_count");
                let bucket_start = from + chrono::Duration::seconds(bucket * bucket_seconds);
                Ok(MotionHistogramBucket {
                    bucket,
                    bucket_start,
                    event_count,
                    clip_count,
                })
            })
            .collect()
    }

    /// Stamp `events.clip_id` for an alert. Called by the supervisor in
    /// the same per-frame transaction as `record_event` once the
    /// recorder hands back the open clip handle for the camera.
    pub async fn link_event_to_clip(
        &self,
        event_id: &str,
        clip_id: ClipId,
    ) -> Result<(), StoreError> {
        let res = sqlx::query("UPDATE events SET clip_id = ? WHERE event_id = ?")
            .bind(clip_id)
            .bind(event_id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("event_id={event_id}")));
        }
        Ok(())
    }

    /// Read back the clip_id stamped on an event row. Returns
    /// `Ok(None)` if the row exists but has no clip cross-reference,
    /// and `Err(NotFound)` if the row itself is missing. Used by the
    /// alert detail UI to deep-link into the surrounding clip and by
    /// the supervisor's end-to-end test to verify the link wired up.
    pub async fn get_event_clip_id(&self, event_id: &str) -> Result<Option<ClipId>, StoreError> {
        let row = sqlx::query("SELECT clip_id FROM events WHERE event_id = ?")
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            None => Err(StoreError::NotFound(format!("event_id={event_id}"))),
            Some(r) => Ok(r.try_get::<Option<i64>, _>(0).ok().flatten()),
        }
    }
}

// ---------------------------------------------------------------------------
// Row decoders
// ---------------------------------------------------------------------------

fn clip_row_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ClipRow, StoreError> {
    let started_at = parse_ts(row.get::<String, _>(2).as_str())?;
    let ended_at = row
        .try_get::<Option<String>, _>(3)?
        .map(|s| parse_ts(&s))
        .transpose()?;
    Ok(ClipRow {
        id: row.get::<i64, _>(0),
        camera_id: row.get::<i64, _>(1),
        started_at,
        ended_at,
        path: row.get::<String, _>(4),
        duration_ms: row.get::<i64, _>(5),
        size_bytes: row.get::<i64, _>(6),
        codec: row.get::<String, _>(7),
        container: row.get::<String, _>(8),
        backend_id: row.get::<String, _>(9),
    })
}

fn motion_event_row_from_row(row: sqlx::sqlite::SqliteRow) -> Result<MotionEventRow, StoreError> {
    let captured_at = parse_ts(row.get::<String, _>(5).as_str())?;
    let kind = MotionEventKind::parse(row.get::<String, _>(4).as_str())?;
    let bbox = BBox {
        x1: row.get::<f64, _>(6) as f32,
        y1: row.get::<f64, _>(7) as f32,
        x2: row.get::<f64, _>(8) as f32,
        y2: row.get::<f64, _>(9) as f32,
    };
    Ok(MotionEventRow {
        id: row.get::<i64, _>(0),
        camera_id: row.get::<i64, _>(1),
        clip_id: row.get::<i64, _>(2),
        track_id: row.get::<i64, _>(3) as u64,
        kind,
        captured_at,
        bbox,
        label: row.get::<String, _>(10),
        confidence: row.get::<f64, _>(11) as f32,
        attributes_json: row.get::<String, _>(12),
    })
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| StoreError::NotFound(format!("bad timestamp '{s}': {e}")))
}
