//! Clip recorder — M2.1 Stage A.
//!
//! The recorder is the bridge between the tracker's lifecycle events
//! (`born` / `updated` / `died`) and the on-disk `motion_clips` rows
//! plus the GStreamer-managed mp4 files.
//!
//! Stage A (this module) ships:
//!
//! * [`ClipRecorder`] — the trait every recorder backend implements.
//! * [`StubClipRecorder`] — the always-available implementation that
//!   creates 0-byte placeholder files and writes the matching
//!   `motion_clips` row. Lets the rest of Stage A (motion-event
//!   emitter, watermark sampler, eviction loop, retention sweeper, API
//!   endpoints) land + be tested without a working GStreamer
//!   pipeline.
//! * [`OpenClip`] / [`ClipFinal`] — the data the supervisor passes
//!   into the recorder when a track is born / dies, and what comes
//!   back when the file is finalised.
//!
//! Stage B will add a real `GstClipRecorder` behind
//! `#[cfg(feature = "gstreamer")]` that owns a `splitmuxsink` per
//! camera and stamps real `duration_ms` / `size_bytes`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use nexus_store::{ClipClose, ClipId, NewClip, Store};
use nexus_types::CameraId;
use thiserror::Error;
use tokio::fs;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// Open a new clip for a camera. The path on disk is recorder-controlled
/// so we don't have to thread filesystem layout through every caller.
#[derive(Debug, Clone)]
pub struct OpenClip {
    pub camera_id: CameraId,
    pub started_at: DateTime<Utc>,
}

/// Hand the closed handle back to the recorder so it can finalise the
/// file and stamp the row.
#[derive(Debug, Clone)]
pub struct ClipFinal {
    pub ended_at: DateTime<Utc>,
}

/// Handle returned by [`ClipRecorder::open`]. Opaque to callers; the
/// only thing they do is hand it back to [`ClipRecorder::close`].
#[derive(Debug, Clone, Copy)]
pub struct ClipHandle {
    pub clip_id: ClipId,
    pub camera_id: CameraId,
}

/// Final state stamped on the `motion_clips` row when [`ClipRecorder::close`]
/// completes. Returned for the supervisor + tests so they don't need a
/// second store roundtrip.
#[derive(Debug, Clone)]
pub struct ClipMeta {
    pub clip_id: ClipId,
    pub camera_id: CameraId,
    pub path: PathBuf,
    pub duration_ms: i64,
    pub size_bytes: i64,
    pub codec: String,
    pub container: String,
}

#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("recorder is panic-paused; new clips refused")]
    Refused,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("store: {0}")]
    Store(#[from] nexus_store::StoreError),
    #[error("clip {0} is not open in this recorder")]
    UnknownClip(ClipId),
}

/// What every recorder implementation has to provide. Stage A only ships
/// [`StubClipRecorder`]; Stage B will add `GstClipRecorder` behind
/// `#[cfg(feature = "gstreamer")]` against the same trait.
#[async_trait]
pub trait ClipRecorder: Send + Sync {
    /// Begin a new clip for the camera. Inserts the `motion_clips` row
    /// and returns the handle the supervisor will hand back to
    /// [`Self::close`] when the track dies (plus post-roll grace).
    ///
    /// Returns [`RecorderError::Refused`] when the watermark sampler
    /// has flipped the recorder into panic mode. Callers MUST treat
    /// that as a normal outcome (drop the motion event entirely; do
    /// NOT emit a clipless `motion_events` row — the schema
    /// invariant is `clip_id NOT NULL`).
    async fn open(&self, args: OpenClip) -> Result<ClipHandle, RecorderError>;

    /// Finalise the clip: close the file, stamp `ended_at` /
    /// `duration_ms` / `size_bytes` on the row.
    async fn close(&self, handle: ClipHandle, args: ClipFinal) -> Result<ClipMeta, RecorderError>;

    /// Set or clear the panic flag. The watermark sampler calls
    /// `set_panic(true)` when free space is below
    /// `panic_watermark_pct` and `set_panic(false)` once eviction has
    /// recovered to the low+5pct threshold.
    fn set_panic(&self, panic: bool);

    /// True iff `set_panic(true)` was the last call (or set in
    /// constructor). Read by tests and by `/api/v1/storage/local`.
    fn is_panic(&self) -> bool;

    /// Identifier for OPS dashboards / health endpoints (`stub`,
    /// `gstreamer`, etc.).
    fn kind(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// StubClipRecorder
// ---------------------------------------------------------------------------

/// Recorder that writes 0-byte placeholder files and the matching
/// `motion_clips` row.
///
/// Behaviour matches [`ClipRecorder`] exactly so the rest of Stage A
/// (motion-event emitter, watermark sampler, eviction loop, retention
/// sweeper, API) can be wired + tested end-to-end without GStreamer.
/// The on-disk file is created so the safety floor's
/// `oldest_clip.path` unlink + the orphan-file scan both have a real
/// inode to work against.
pub struct StubClipRecorder {
    store: Arc<Store>,
    clips_dir: PathBuf,
    panic: parking_lot::RwLock<bool>,
    /// Tracks open clips so [`Self::close`] knows which `started_at` /
    /// `path` belongs to a handle without doing a second store
    /// roundtrip. Keyed by `clip_id`.
    open: Mutex<std::collections::HashMap<ClipId, OpenState>>,
}

#[derive(Debug, Clone)]
struct OpenState {
    camera_id: CameraId,
    started_at: DateTime<Utc>,
    path: PathBuf,
}

impl StubClipRecorder {
    pub fn new(store: Arc<Store>, clips_dir: impl AsRef<Path>) -> Self {
        Self {
            store,
            clips_dir: clips_dir.as_ref().to_path_buf(),
            panic: parking_lot::RwLock::new(false),
            open: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Path the recorder would write for `(camera_id, started_at)`.
    /// Stable + collision-resistant across same-second opens by
    /// including the unix-millis suffix.
    pub fn clip_path(&self, camera_id: CameraId, started_at: DateTime<Utc>) -> PathBuf {
        let ts = started_at.format("%Y%m%dT%H%M%S");
        let ms = started_at.timestamp_subsec_millis();
        self.clips_dir
            .join(format!("cam{camera_id}"))
            .join(format!("{ts}_{ms:03}.mp4"))
    }
}

#[async_trait]
impl ClipRecorder for StubClipRecorder {
    async fn open(&self, args: OpenClip) -> Result<ClipHandle, RecorderError> {
        if *self.panic.read() {
            return Err(RecorderError::Refused);
        }

        let path = self.clip_path(args.camera_id, args.started_at);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        // Touch a 0-byte file so the eviction unlink + orphan-file scan
        // both have a real inode to work against.
        fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .await?;

        let rel = path
            .strip_prefix(&self.clips_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());

        let new = NewClip {
            camera_id: args.camera_id,
            started_at: args.started_at,
            path: rel,
            codec: "stub".into(),
            container: "mp4".into(),
            backend_id: "local".into(),
        };
        let clip_id = self.store.open_clip(&new).await?;

        self.open.lock().await.insert(
            clip_id,
            OpenState {
                camera_id: args.camera_id,
                started_at: args.started_at,
                path,
            },
        );
        debug!(
            camera_id = args.camera_id,
            clip_id, "stub recorder opened clip"
        );
        Ok(ClipHandle {
            clip_id,
            camera_id: args.camera_id,
        })
    }

    async fn close(&self, handle: ClipHandle, args: ClipFinal) -> Result<ClipMeta, RecorderError> {
        let state = self
            .open
            .lock()
            .await
            .remove(&handle.clip_id)
            .ok_or(RecorderError::UnknownClip(handle.clip_id))?;

        let duration_ms = (args.ended_at - state.started_at).num_milliseconds().max(0);
        let size_bytes = match fs::metadata(&state.path).await {
            Ok(meta) => meta.len() as i64,
            Err(e) => {
                warn!(
                    error = %e,
                    path = %state.path.display(),
                    "stub recorder could not stat clip; recording size_bytes=0"
                );
                0
            }
        };

        self.store
            .close_clip(
                handle.clip_id,
                &ClipClose {
                    ended_at: args.ended_at,
                    duration_ms,
                    size_bytes,
                },
            )
            .await?;

        debug!(
            camera_id = state.camera_id,
            clip_id = handle.clip_id,
            duration_ms,
            "stub recorder closed clip"
        );
        Ok(ClipMeta {
            clip_id: handle.clip_id,
            camera_id: state.camera_id,
            path: state.path,
            duration_ms,
            size_bytes,
            codec: "stub".into(),
            container: "mp4".into(),
        })
    }

    fn set_panic(&self, panic: bool) {
        let mut guard = self.panic.write();
        if *guard != panic {
            tracing::info!(panic, "stub recorder panic flag changed");
        }
        *guard = panic;
    }

    fn is_panic(&self) -> bool {
        *self.panic.read()
    }

    fn kind(&self) -> &'static str {
        "stub"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use nexus_config::{CameraConfig, StoreConfig};
    use nexus_store::Store;
    use std::path::PathBuf;
    use url::Url;

    async fn fresh_store_and_dir() -> (Arc<Store>, tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tmpdir");
        let db_path = dir.path().join("nexus.db");
        let cfg = StoreConfig {
            url: format!("sqlite:{}?mode=rwc", db_path.display()),
            seed_from_config: false,
            duckdb_attach: false,
            duckdb_path: PathBuf::from("/tmp/unused.duckdb"),
        };
        let store = Arc::new(Store::open(&cfg).await.expect("store"));
        store
            .upsert_camera(&CameraConfig {
                id: 1,
                name: "front".into(),
                url: Url::parse("rtsp://127.0.0.1/stream").unwrap(),
                enabled: true,
                prompts: vec![],
                model_override: None,
                zones: vec![],
                max_fps: 0,
                parking_lot_mode: false,
            })
            .await
            .unwrap();
        let clips_dir = dir.path().join("clips");
        (store, dir, clips_dir)
    }

    #[tokio::test]
    async fn stub_recorder_open_close_round_trip() {
        let (store, _dir, clips_dir) = fresh_store_and_dir().await;
        let rec = StubClipRecorder::new(store.clone(), &clips_dir);

        let started = Utc::now();
        let handle = rec
            .open(OpenClip {
                camera_id: 1,
                started_at: started,
            })
            .await
            .unwrap();
        assert_eq!(rec.kind(), "stub");

        // File exists on disk + clip row exists in the DB.
        let row = store.get_clip(handle.clip_id).await.unwrap().unwrap();
        let path_on_disk = clips_dir.join(&row.path);
        assert!(path_on_disk.exists(), "stub clip file should be created");
        assert_eq!(row.codec, "stub");
        assert_eq!(row.backend_id, "local");
        assert!(row.ended_at.is_none(), "ended_at unset until close");

        let ended = started + chrono::Duration::seconds(7);
        let meta = rec
            .close(handle, ClipFinal { ended_at: ended })
            .await
            .unwrap();
        assert_eq!(meta.duration_ms, 7_000);
        assert_eq!(meta.size_bytes, 0); // touch-only file

        let row_closed = store.get_clip(handle.clip_id).await.unwrap().unwrap();
        assert_eq!(row_closed.duration_ms, 7_000);
        assert_eq!(row_closed.size_bytes, 0);
        assert!(row_closed.ended_at.is_some());
    }

    #[tokio::test]
    async fn stub_recorder_panic_mode_refuses_open_but_allows_close() {
        let (store, _dir, clips_dir) = fresh_store_and_dir().await;
        let rec = StubClipRecorder::new(store.clone(), &clips_dir);

        // Open one clip BEFORE panic so we have an in-flight handle.
        let started = Utc::now();
        let handle = rec
            .open(OpenClip {
                camera_id: 1,
                started_at: started,
            })
            .await
            .unwrap();

        // Flip panic. New opens must be refused.
        rec.set_panic(true);
        assert!(rec.is_panic());
        let refused = rec
            .open(OpenClip {
                camera_id: 1,
                started_at: started + chrono::Duration::seconds(1),
            })
            .await;
        assert!(matches!(refused, Err(RecorderError::Refused)));

        // Existing handle MUST still close cleanly so the in-flight
        // clip's row gets ended_at + size stamped.
        let meta = rec
            .close(
                handle,
                ClipFinal {
                    ended_at: started + chrono::Duration::seconds(2),
                },
            )
            .await
            .unwrap();
        assert_eq!(meta.duration_ms, 2_000);

        // Clearing panic re-enables opens.
        rec.set_panic(false);
        assert!(!rec.is_panic());
        let _h2 = rec
            .open(OpenClip {
                camera_id: 1,
                started_at: started + chrono::Duration::seconds(3),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stub_recorder_close_unknown_handle_errors() {
        let (store, _dir, clips_dir) = fresh_store_and_dir().await;
        let rec = StubClipRecorder::new(store, &clips_dir);
        let bogus = ClipHandle {
            clip_id: 9999,
            camera_id: 1,
        };
        let res = rec
            .close(
                bogus,
                ClipFinal {
                    ended_at: Utc::now(),
                },
            )
            .await;
        assert!(matches!(res, Err(RecorderError::UnknownClip(9999))));
    }

    #[test]
    fn clip_path_is_stable_and_per_camera() {
        // Doesn't need the store, just exercises the path layout.
        let dummy_store = {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let dir = tempfile::tempdir().unwrap();
                let cfg = StoreConfig {
                    url: format!("sqlite:{}?mode=rwc", dir.path().join("x.db").display()),
                    seed_from_config: false,
                    duckdb_attach: false,
                    duckdb_path: PathBuf::from("/tmp/unused.duckdb"),
                };
                let s = Arc::new(Store::open(&cfg).await.unwrap());
                // Leak the tempdir so the file path stays valid for the
                // assertions that follow.
                std::mem::forget(dir);
                s
            })
        };
        let rec = StubClipRecorder::new(dummy_store, "/var/lib/nexus/clips");
        let t = chrono::DateTime::parse_from_rfc3339("2026-05-13T22:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        let p1 = rec.clip_path(1, t);
        let p2 = rec.clip_path(2, t);
        assert!(p1.to_string_lossy().contains("cam1"));
        assert!(p2.to_string_lossy().contains("cam2"));
        assert!(p1.to_string_lossy().ends_with("20260513T220000_123.mp4"));
    }
}
